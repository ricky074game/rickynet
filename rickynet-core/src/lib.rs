//! rickynet-core — the RickyNet phone data plane, exposed to the Swift app over
//! a tiny C ABI (`rn_start` / `rn_stop` / `rn_stats`).
//!
//! Responsibilities:
//!   1. Listen for the desktop's connection (loopback for usbmux/USB, or the
//!      LAN address for Wi-Fi) and read `[u16 len][IP packet]` frames off it.
//!   2. Inject those packets into a userspace TCP/IP stack (`ipstack`) that
//!      terminates each flow.
//!   3. Re-originate every terminated flow as a REAL OS socket bound to the
//!      cellular interface (see `egress`), so the desktop's traffic egresses as
//!      this app's own cellular sockets — ordinary phone-data usage, not
//!      carrier-metered tethering.
//!
//! The C ABI is intentionally minimal; all lifetime/threading lives here. The
//! header `rickynetcore.h` is generated from these signatures by cbindgen.

mod device;
mod egress;
mod flow;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as stdmpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use ipstack::{IpStack, IpStackConfig, IpStackStream, TcpConfig, TcpOptions};
use tokio::runtime::Builder;
use tokio::sync::watch;

use device::FramedDevice;
use rickynet_wire::{TRANSPORT_USB, TRANSPORT_WIFI};

// Cumulative byte counters surfaced via `rn_stats`. Reset on each `rn_start`.
pub(crate) static RX_BYTES: AtomicU64 = AtomicU64::new(0);
pub(crate) static TX_BYTES: AtomicU64 = AtomicU64::new(0);

struct Instance {
    shutdown: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

// Global singleton: RickyNet runs one listener at a time.
static INSTANCE: Mutex<Option<Instance>> = Mutex::new(None);

// Return codes for the C ABI.
const RN_OK: i32 = 0;
const RN_ERR_NOT_RUNNING: i32 = -1;
const RN_ERR_ALREADY_RUNNING: i32 = -2;
const RN_ERR_BIND: i32 = -3;
const RN_ERR_RUNTIME: i32 = -4;

/// Start the listener + netstack loop.
///
/// * `port`      — TCP port to listen on (see `rickynet_wire::DEFAULT_PORT`).
/// * `transport` — `0` (usbmux/USB: bind 127.0.0.1) or `1` (Wi-Fi: bind 0.0.0.0).
///
/// Returns `0` on success (listener bound), or a negative error code. Blocks
/// only until the socket is bound; the netstack then runs on its own threads.
#[no_mangle]
pub extern "C" fn rn_start(port: u16, transport: u32) -> i32 {
    let mut guard = match INSTANCE.lock() {
        Ok(g) => g,
        Err(_) => return RN_ERR_RUNTIME,
    };
    if guard.is_some() {
        return RN_ERR_ALREADY_RUNNING;
    }

    RX_BYTES.store(0, Ordering::Relaxed);
    TX_BYTES.store(0, Ordering::Relaxed);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = stdmpsc::channel::<Result<(), String>>();

    // Don't `.expect()` here: a panic would cross the extern "C" boundary and,
    // with panic="abort", hard-kill the iOS app. Return an error code instead.
    let handle = match std::thread::Builder::new()
        .name("rickynet-core".into())
        .spawn(move || run_runtime(port, transport, ready_tx, shutdown_rx))
    {
        Ok(h) => h,
        Err(e) => {
            log::error!("rn_start: failed to spawn core thread: {e}");
            return RN_ERR_RUNTIME;
        }
    };

    // Wait for the bind result so we can report a real success/failure.
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            *guard = Some(Instance {
                shutdown: shutdown_tx,
                handle,
            });
            RN_OK
        }
        Ok(Err(e)) => {
            log::error!("rn_start: bind failed: {e}");
            let _ = handle.join();
            RN_ERR_BIND
        }
        Err(_) => {
            log::error!("rn_start: runtime did not report readiness");
            let _ = shutdown_tx.send(true);
            let _ = handle.join();
            RN_ERR_RUNTIME
        }
    }
}

/// Stop the listener and tear everything down. Returns `0`, or `-1` if not running.
#[no_mangle]
pub extern "C" fn rn_stop() -> i32 {
    let instance = match INSTANCE.lock() {
        Ok(mut g) => g.take(),
        Err(_) => return RN_ERR_RUNTIME,
    };
    match instance {
        Some(inst) => {
            let _ = inst.shutdown.send(true);
            let _ = inst.handle.join();
            RN_OK
        }
        None => RN_ERR_NOT_RUNNING,
    }
}

/// Write cumulative byte counters. Either pointer may be null.
///   * `out_rx` — bytes downloaded from the internet for the desktop.
///   * `out_tx` — bytes uploaded from the desktop to the internet.
#[no_mangle]
pub extern "C" fn rn_stats(out_rx: *mut u64, out_tx: *mut u64) {
    if !out_rx.is_null() {
        unsafe { *out_rx = RX_BYTES.load(Ordering::Relaxed) };
    }
    if !out_tx.is_null() {
        unsafe { *out_tx = TX_BYTES.load(Ordering::Relaxed) };
    }
}

/// Thread entry: build a Tokio runtime and run the server to completion.
fn run_runtime(
    port: u16,
    transport: u32,
    ready_tx: stdmpsc::Sender<Result<(), String>>,
    shutdown_rx: watch::Receiver<bool>,
) {
    let rt = match Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("tokio runtime: {e}")));
            return;
        }
    };
    rt.block_on(async move {
        serve(port, transport, ready_tx, shutdown_rx).await;
    });
}

/// Bind the listener, report readiness, then accept desktop connections one at a
/// time until shutdown. Each accepted connection is bridged through its own
/// `ipstack` instance; when the desktop disconnects we loop and accept again.
async fn serve(
    port: u16,
    transport: u32,
    ready_tx: stdmpsc::Sender<Result<(), String>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let bind_ip = match transport {
        TRANSPORT_USB => "127.0.0.1",
        TRANSPORT_WIFI => "0.0.0.0",
        other => {
            let _ = ready_tx.send(Err(format!("unknown transport {other}")));
            return;
        }
    };
    let addr = format!("{bind_ip}:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("bind {addr}: {e}")));
            return;
        }
    };
    log::info!("RickyNet core listening on {addr} (transport {transport})");
    let _ = ready_tx.send(Ok(()));

    loop {
        tokio::select! {
            _ = wait_shutdown(&mut shutdown_rx) => {
                log::info!("core: shutdown requested");
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => { log::warn!("accept error: {e}"); continue; }
                };
                log::info!("desktop connected from {peer}");
                let mut sd = shutdown_rx.clone();
                tokio::select! {
                    _ = wait_shutdown(&mut sd) => break,
                    _ = bridge(stream) => {
                        log::info!("desktop disconnected; awaiting reconnect");
                    }
                }
            }
        }
    }
}

/// Resolve when the shutdown flag flips to `true` (handles the already-set case).
async fn wait_shutdown(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

/// Drive one desktop connection: feed its framed IP packets into `ipstack` and
/// hand each accepted flow to a per-flow handler. Returns when the desktop
/// disconnects (via the device's disconnect signal) so `serve()` can accept the
/// next connection.
async fn bridge(stream: tokio::net::TcpStream) {
    let (device, mut disconnected) = FramedDevice::new(stream);

    let mut config = IpStackConfig::default();
    // Large device MTU so any single desktop packet fits in one read; TCP is
    // kept polite to the desktop path via an explicit MSS clamp below.
    let _ = config.mtu(u16::MAX);
    let mut tcp_config = TcpConfig::default();
    tcp_config.timeout = Duration::from_secs(60);
    // 1360 = 1400 (tunnel MTU) - 20 (IP) - 20 (TCP); keeps desktop-bound
    // segments within the Wintun path so nothing needs fragmenting.
    tcp_config.options = Some(vec![TcpOptions::MaximumSegmentSize(1360)]);
    tcp_config.max_unacked_bytes = 256 * 1024;
    config.with_tcp_config(tcp_config);
    config.udp_timeout(Duration::from_secs(30));

    let mut ip_stack = IpStack::new(config, device);

    loop {
        tokio::select! {
            // Desktop disconnected: drop `ip_stack` (its internal task aborts)
            // and return so serve() can accept a reconnect.
            _ = &mut disconnected => {
                log::debug!("desktop disconnected; tearing down ipstack");
                break;
            }
            accepted = ip_stack.accept() => {
                match accepted {
                    Ok(IpStackStream::Tcp(tcp)) => {
                        tokio::spawn(flow::handle_tcp(tcp));
                    }
                    Ok(IpStackStream::Udp(udp)) => {
                        tokio::spawn(flow::handle_udp(udp));
                    }
                    Ok(IpStackStream::UnknownTransport(u)) => {
                        tokio::spawn(flow::handle_unknown(u));
                    }
                    Ok(IpStackStream::UnknownNetwork(_)) => {}
                    Err(e) => {
                        log::debug!("ipstack accept ended: {e}");
                        break;
                    }
                }
            }
        }
    }
}
