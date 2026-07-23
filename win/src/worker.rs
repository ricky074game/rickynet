//! The background worker: brings the tunnel up, runs the packet pump, and tears
//! everything down cleanly. Nothing here touches the GUI directly — it only
//! reads/writes the shared `state::Shared`.
//!
//! Data path (both directions are one dedicated blocking thread):
//!   Wintun receive_blocking -> frame -> transport   (desktop upload,   TX)
//!   transport read_frame     -> Wintun send_packet  (internet download, RX)

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rickynet_wire::{read_frame, write_frame};

use crate::args::Args;
use crate::routing::Routes;
use crate::state::{ConnState, Shared};
use crate::transport::{self, TransportKind};
use crate::tun::Tun;

// Tunnel-side addressing (private, link-local-ish /24 just for the adapter).
const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(10, 6, 0, 2);
const TUN_MASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
const TUN_MTU: usize = 1400;

/// Bring the tunnel up and pump until `shared` is asked to stop (or a fatal
/// error). Returns when everything is torn down. Runs on its own thread.
pub fn run_connect(shared: Arc<Shared>, args: Args) {
    log::info!(
        "worker: connect requested (transport {}, port {}, phone_ip {:?}, adapter '{}')",
        args.transport.label(),
        args.port,
        args.phone_ip,
        args.adapter_name
    );
    shared.clear_stop();
    shared.reset_counters();
    shared.set_state(ConnState::Connecting);

    if let Err(e) = connect_inner(&shared, &args) {
        log::error!("worker: connect failed: {e} (kind {:?})", e.kind());
        shared.set_error(format!("{e}"));
    }

    // Whatever happened, we've returned to a torn-down state.
    if shared.state() != ConnState::Error {
        shared.set_state(ConnState::Disconnected);
    }
    shared.log("disconnected");
}

fn connect_inner(shared: &Arc<Shared>, args: &Args) -> std::io::Result<()> {
    // 1. Adapter up.
    shared.log("creating adapter…");
    let t0 = Instant::now();
    let dns = [
        std::net::IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        std::net::IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
    ];
    let tun = Tun::up(&args.adapter_name, TUN_ADDR, TUN_MASK, &dns, TUN_MTU)?;
    shared.log(format!(
        "adapter up in {:.1?} ({TUN_ADDR}/24, MTU {TUN_MTU}, DNS 1.1.1.1/8.8.8.8)",
        t0.elapsed()
    ));

    // 2. Transport (before routes, so the Wi-Fi socket forms on the real net).
    shared.log(format!("connecting to phone over {}…", args.transport.label()));
    let t1 = Instant::now();
    let stream = transport::connect(args.transport, args.phone_ip.as_deref(), args.port)?;
    shared.log(format!("connected to device in {:.1?}", t1.elapsed()));

    // 3. Routes: capture all traffic; carve out the phone IP for Wi-Fi.
    let carve = match args.transport {
        TransportKind::Wifi => args.phone_ip.as_deref().and_then(|s| s.parse::<Ipv4Addr>().ok()),
        TransportKind::Usb => None,
    };
    let routes = Routes::install(tun.luid(), carve)?;
    shared.log("routes set — all traffic via tunnel");

    shared.set_state(ConnState::Connected);
    log::info!("worker: tunnel established, pump starting");

    // 4. Pump.
    let read_stream = stream.try_clone()?;
    let write_stream = stream.try_clone()?;
    let shutdown_stream = stream;

    let alive = Arc::new(AtomicBool::new(true));
    let tx_pkts = Arc::new(AtomicU64::new(0));
    let rx_pkts = Arc::new(AtomicU64::new(0));

    // Thread A: Wintun -> transport (TX / upload).
    let a = {
        let session = tun.session();
        let shared = shared.clone();
        let alive = alive.clone();
        let tx_pkts = tx_pkts.clone();
        let mut out = write_stream;
        std::thread::spawn(move || {
            let reason = loop {
                let packet = match session.receive_blocking() {
                    Ok(p) => p,
                    Err(e) => break format!("Wintun session ended: {e}"),
                };
                let bytes = packet.bytes();
                if let Err(e) = write_frame(&mut out, bytes) {
                    break format!("transport write failed: {e}");
                }
                shared.tx.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if tx_pkts.fetch_add(1, Ordering::Relaxed) == 0 {
                    log::debug!("pump: first upload packet ({} bytes)", bytes.len());
                }
            };
            log::info!(
                "pump TX (Wintun→phone) done after {} packets ({reason})",
                tx_pkts.load(Ordering::Relaxed)
            );
            alive.store(false, Ordering::Relaxed);
        })
    };

    // Thread B: transport -> Wintun (RX / download).
    let b = {
        let session = tun.session();
        let shared = shared.clone();
        let alive = alive.clone();
        let rx_pkts = rx_pkts.clone();
        let mut inp = read_stream;
        std::thread::spawn(move || {
            let reason = loop {
                let pkt = match read_frame(&mut inp) {
                    Ok(p) => p,
                    Err(e) => break format!("transport read ended: {e}"),
                };
                if pkt.is_empty() || pkt.len() > u16::MAX as usize {
                    log::warn!("pump: skipping bogus frame of {} bytes", pkt.len());
                    continue;
                }
                match session.allocate_send_packet(pkt.len() as u16) {
                    Ok(mut send) => {
                        send.bytes_mut().copy_from_slice(&pkt);
                        session.send_packet(send);
                        shared.rx.fetch_add(pkt.len() as u64, Ordering::Relaxed);
                        if rx_pkts.fetch_add(1, Ordering::Relaxed) == 0 {
                            log::debug!("pump: first download packet ({} bytes)", pkt.len());
                        }
                    }
                    Err(e) => break format!("Wintun allocate failed: {e}"),
                }
            };
            log::info!(
                "pump RX (phone→Wintun) done after {} packets ({reason})",
                rx_pkts.load(Ordering::Relaxed)
            );
            alive.store(false, Ordering::Relaxed);
        })
    };

    // 5. Wait for stop or a dead pump; log a traffic heartbeat every 15 s so a
    // stalled link is visible in the log even when nothing errors.
    let mut last_beat = Instant::now();
    while !shared.should_stop() && alive.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        if last_beat.elapsed() >= Duration::from_secs(15) {
            last_beat = Instant::now();
            log::debug!(
                "heartbeat: rx {} B / {} pkts, tx {} B / {} pkts",
                shared.rx(),
                rx_pkts.load(Ordering::Relaxed),
                shared.tx(),
                tx_pkts.load(Ordering::Relaxed)
            );
        }
    }
    log::info!(
        "worker: leaving pump loop (stop_requested={}, pump_alive={})",
        shared.should_stop(),
        alive.load(Ordering::Relaxed)
    );

    // 6. Teardown: unblock both threads, join, remove routes, drop adapter.
    shared.log("stopping…");
    tun.stop_session(); // unblocks thread A (receive_blocking)
    let _ = shutdown_stream.shutdown(std::net::Shutdown::Both); // unblocks thread B (read)
    let _ = a.join();
    let _ = b.join();
    log::debug!("worker: pump threads joined");
    routes.remove(); // removes split-default AND the Wi-Fi /32 carve-out
    log::debug!("worker: routes removed");
    drop(tun); // WintunCloseAdapter -> adapter + its address/DNS gone
    log::debug!("worker: adapter closed");
    shared.log(format!(
        "session totals: down {} B, up {} B",
        shared.rx(),
        shared.tx()
    ));
    Ok(())
}

// --- Headless mode (Ctrl+C aware) --------------------------------------------

static HEADLESS_SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

/// Run the bridge without a GUI until Ctrl+C. Assumes we are already elevated.
pub fn run_headless(args: &Args) {
    let shared = Shared::new();
    shared.set_admin(true);
    let _ = HEADLESS_SHARED.set(shared.clone());
    install_ctrl_handler();

    println!("RickyNet (headless): {} transport, port {}", args.transport.label(), args.port);
    println!("Press Ctrl+C to stop.");

    run_connect(shared.clone(), args.clone());

    for line in shared.logs() {
        println!("  {line}");
    }
    match shared.state() {
        ConnState::Error => {
            eprintln!("exited with error: {}", shared.error());
            std::process::exit(1);
        }
        _ => println!("stopped cleanly."),
    }
}

fn install_ctrl_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_ctrl_type: u32) -> windows_sys::Win32::Foundation::BOOL {
        if let Some(s) = HEADLESS_SHARED.get() {
            s.request_stop();
        }
        1 // TRUE: handled
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}
