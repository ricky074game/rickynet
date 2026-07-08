//! The background worker: brings the tunnel up, runs the packet pump, and tears
//! everything down cleanly. Nothing here touches the GUI directly — it only
//! reads/writes the shared `state::Shared`.
//!
//! Data path (both directions are one dedicated blocking thread):
//!   Wintun receive_blocking -> frame -> transport   (desktop upload,   TX)
//!   transport read_frame     -> Wintun send_packet  (internet download, RX)

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

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
    shared.clear_stop();
    shared.reset_counters();
    shared.set_state(ConnState::Connecting);

    if let Err(e) = connect_inner(&shared, &args) {
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
    let dns = [
        std::net::IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        std::net::IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
    ];
    let tun = Tun::up(&args.adapter_name, TUN_ADDR, TUN_MASK, &dns, TUN_MTU)?;
    shared.log("adapter up (10.6.0.2/24, DNS 1.1.1.1)");

    // 2. Transport (before routes, so the Wi-Fi socket forms on the real net).
    shared.log(format!("connecting to phone over {}…", args.transport.label()));
    let stream = transport::connect(args.transport, args.phone_ip.as_deref(), args.port)?;
    shared.log("connected to device");

    // 3. Routes: capture all traffic; carve out the phone IP for Wi-Fi.
    let carve = match args.transport {
        TransportKind::Wifi => args.phone_ip.as_deref().and_then(|s| s.parse::<Ipv4Addr>().ok()),
        TransportKind::Usb => None,
    };
    let routes = Routes::install(tun.luid(), carve)?;
    shared.log("routes set — all traffic via tunnel");

    shared.set_state(ConnState::Connected);

    // 4. Pump.
    let read_stream = stream.try_clone()?;
    let write_stream = stream.try_clone()?;
    let shutdown_stream = stream;

    let alive = Arc::new(AtomicBool::new(true));

    // Thread A: Wintun -> transport (TX / upload).
    let a = {
        let session = tun.session();
        let shared = shared.clone();
        let alive = alive.clone();
        let mut out = write_stream;
        std::thread::spawn(move || {
            loop {
                let packet = match session.receive_blocking() {
                    Ok(p) => p,
                    Err(_) => break, // session shut down
                };
                let bytes = packet.bytes();
                if write_frame(&mut out, bytes).is_err() {
                    break;
                }
                shared.tx.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            }
            alive.store(false, Ordering::Relaxed);
        })
    };

    // Thread B: transport -> Wintun (RX / download).
    let b = {
        let session = tun.session();
        let shared = shared.clone();
        let alive = alive.clone();
        let mut inp = read_stream;
        std::thread::spawn(move || {
            loop {
                let pkt = match read_frame(&mut inp) {
                    Ok(p) => p,
                    Err(_) => break, // transport closed
                };
                if pkt.is_empty() || pkt.len() > u16::MAX as usize {
                    continue;
                }
                match session.allocate_send_packet(pkt.len() as u16) {
                    Ok(mut send) => {
                        send.bytes_mut().copy_from_slice(&pkt);
                        session.send_packet(send);
                        shared.rx.fetch_add(pkt.len() as u64, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
            alive.store(false, Ordering::Relaxed);
        })
    };

    // 5. Wait for stop or a dead pump.
    while !shared.should_stop() && alive.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }

    // 6. Teardown: unblock both threads, join, remove routes, drop adapter.
    shared.log("stopping…");
    tun.stop_session(); // unblocks thread A (receive_blocking)
    let _ = shutdown_stream.shutdown(std::net::Shutdown::Both); // unblocks thread B (read)
    let _ = a.join();
    let _ = b.join();
    routes.remove(); // removes split-default AND the Wi-Fi /32 carve-out
    drop(tun); // WintunCloseAdapter -> adapter + its address/DNS gone
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
