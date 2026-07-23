//! Wintun adapter wrapper. Creating the adapter requires Administrator.
//!
//! The adapter auto-closes when this `Tun` drops (the wintun crate calls
//! WintunCloseAdapter in its `Drop`), which also removes the routes/DNS/address
//! we set on it — so teardown is just: shut down the session, join the pump
//! threads, drop the `Tun`.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;

/// Wintun ring buffer size: 4 MiB (power of two, within MIN/MAX_RING_CAPACITY).
const RING_CAPACITY: u32 = 0x0040_0000;

pub struct Tun {
    _wintun: wintun::Wintun,
    _adapter: Arc<wintun::Adapter>,
    session: Arc<wintun::Session>,
    luid: u64,
}

fn map_err(ctx: &'static str) -> impl Fn(wintun::Error) -> io::Error {
    move |e| io::Error::new(io::ErrorKind::Other, format!("{ctx}: {e}"))
}

/// wintun.dll is expected next to rickynet.exe.
fn dll_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wintun.dll")))
        .unwrap_or_else(|| PathBuf::from("wintun.dll"))
}

impl Tun {
    pub fn up(
        name: &str,
        address: Ipv4Addr,
        mask: Ipv4Addr,
        dns: &[IpAddr],
        mtu: usize,
    ) -> io::Result<Tun> {
        let dll = dll_path();
        log::debug!("tun: loading {}", dll.display());
        let wintun = unsafe { wintun::load_from_path(&dll) }.map_err(|e| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "could not load {} — make sure wintun.dll ships next to rickynet.exe ({e})",
                    dll.display()
                ),
            )
        })?;

        // Reuse an existing "RickyNet" adapter if present, else create one. This
        // keeps a crash from accumulating adapters.
        let adapter = match wintun::Adapter::open(&wintun, name) {
            Ok(a) => {
                log::info!("tun: reusing existing adapter '{name}'");
                a
            }
            Err(open_err) => {
                log::debug!("tun: no existing adapter '{name}' ({open_err}); creating");
                let a = wintun::Adapter::create(&wintun, name, "RickyNet", None)
                    .map_err(map_err("create Wintun adapter"))?;
                log::info!("tun: created adapter '{name}'");
                a
            }
        };

        adapter
            .set_network_addresses_tuple(IpAddr::V4(address), IpAddr::V4(mask), None)
            .map_err(map_err("set adapter address"))?;
        log::debug!("tun: address {address}/{mask} set");
        if !dns.is_empty() {
            match adapter.set_dns_servers(dns) {
                Ok(()) => log::debug!("tun: DNS servers set to {dns:?}"),
                Err(e) => log::warn!("set DNS servers failed (continuing): {e}"),
            }
        }
        match adapter.set_mtu(mtu) {
            Ok(()) => log::debug!("tun: MTU set to {mtu}"),
            Err(e) => log::warn!("set MTU failed (continuing): {e}"),
        }

        let luid = unsafe { adapter.get_luid().Value };
        let session = Arc::new(
            adapter
                .start_session(RING_CAPACITY)
                .map_err(map_err("start Wintun session"))?,
        );
        log::info!("tun: session started (luid {luid:#x}, ring {RING_CAPACITY} bytes)");

        Ok(Tun {
            _wintun: wintun,
            _adapter: adapter,
            session,
            luid,
        })
    }

    /// Tunnel adapter LUID (for IP Helper route calls).
    pub fn luid(&self) -> u64 {
        self.luid
    }

    /// Cloneable session handle for the pump threads.
    pub fn session(&self) -> Arc<wintun::Session> {
        self.session.clone()
    }

    /// Unblock any thread parked in `receive_blocking` so the pump can exit.
    pub fn stop_session(&self) {
        let _ = self.session.shutdown();
    }
}
