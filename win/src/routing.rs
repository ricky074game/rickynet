//! Route table management via the IP Helper API (iphlpapi).
//!
//! Capture ALL traffic with the classic split-default trick: two on-link routes
//! `0.0.0.0/1` and `128.0.0.0/1` on the Wintun adapter. These are more specific
//! than the system default `0.0.0.0/0`, so longest-prefix match sends everything
//! through the tunnel — WITHOUT deleting the real default route, so restoring on
//! exit is just deleting the two routes we added.
//!
//! For USB transport no carve-out is needed (the phone link rides loopback via
//! usbmux, which the tunnel routes don't touch). For Wi-Fi transport the phone's
//! LAN IP WOULD get captured, so we pin a `/32` host route for it through the
//! real gateway/interface (discovered with GetBestRoute2) before adding the
//! tunnel routes, and remove it on teardown.

use std::io;
use std::net::Ipv4Addr;

use windows_sys::Win32::Foundation::NO_ERROR;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CreateIpForwardEntry2, DeleteIpForwardEntry2, GetBestRoute2, InitializeIpForwardEntry,
    MIB_IPFORWARD_ROW2,
};
use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_INET};

/// Build an AF_INET `SOCKADDR_INET` for the given IPv4 octets.
fn sockaddr_v4(octets: [u8; 4]) -> SOCKADDR_INET {
    let mut a: SOCKADDR_INET = unsafe { core::mem::zeroed() };
    unsafe {
        a.Ipv4.sin_family = AF_INET;
        // S_addr holds the 4 address bytes in network order; from_ne_bytes
        // reproduces exactly those bytes in memory.
        a.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(octets);
    }
    a
}

fn err(rc: u32) -> io::Error {
    io::Error::from_raw_os_error(rc as i32)
}

/// A set of routes we created; call `remove()` to restore the table on exit.
pub struct Routes {
    rows: Vec<MIB_IPFORWARD_ROW2>,
}

impl Routes {
    /// Set up capture routes on the tunnel adapter (`tun_luid`). If
    /// `carve_out` is `Some(phone_ipv4)`, first pin a /32 for it off-tunnel.
    pub fn install(tun_luid: u64, carve_out: Option<Ipv4Addr>) -> io::Result<Routes> {
        let mut rows: Vec<MIB_IPFORWARD_ROW2> = Vec::new();

        // Wi-Fi carve-out FIRST, so the phone stays reachable off-tunnel.
        if let Some(ip) = carve_out {
            match best_route_to(ip.octets()) {
                Ok(hop) => {
                    match add_gateway_route(hop.iface_luid, ip.octets(), 32, hop.gateway, 1) {
                        Ok(row) => rows.push(row),
                        Err(e) => log::warn!("carve-out /32 for {ip} failed: {e}"),
                    }
                }
                Err(e) => log::warn!("best-route lookup for {ip} failed: {e} (Wi-Fi may drop)"),
            }
        }

        // Split-default on the tunnel adapter (on-link).
        match add_onlink_route(tun_luid, [0, 0, 0, 0], 1, 5) {
            Ok(row) => rows.push(row),
            Err(e) => {
                let r = Routes { rows };
                r.remove();
                return Err(e);
            }
        }
        match add_onlink_route(tun_luid, [128, 0, 0, 0], 1, 5) {
            Ok(row) => rows.push(row),
            Err(e) => {
                let r = Routes { rows };
                r.remove();
                return Err(e);
            }
        }

        Ok(Routes { rows })
    }

    /// Delete every route we added. Best-effort (logs failures).
    pub fn remove(self) {
        for row in &self.rows {
            let rc = unsafe { DeleteIpForwardEntry2(row) };
            if rc != NO_ERROR {
                log::warn!("delete route failed: {}", err(rc));
            }
        }
    }
}

struct BestHop {
    gateway: [u8; 4],
    iface_luid: u64,
    #[allow(dead_code)]
    iface_index: u32,
}

/// Discover the current best next-hop toward `dest` on the physical network,
/// before we install the tunnel routes.
fn best_route_to(dest: [u8; 4]) -> io::Result<BestHop> {
    unsafe {
        let dst = sockaddr_v4(dest);
        let mut row: MIB_IPFORWARD_ROW2 = core::mem::zeroed();
        let mut best_src: SOCKADDR_INET = core::mem::zeroed();
        let rc = GetBestRoute2(
            core::ptr::null(),
            0,
            core::ptr::null(),
            &dst,
            0,
            &mut row,
            &mut best_src,
        );
        if rc != NO_ERROR {
            return Err(err(rc));
        }
        let gateway = row.NextHop.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes();
        Ok(BestHop {
            gateway,
            iface_luid: row.InterfaceLuid.Value,
            iface_index: row.InterfaceIndex,
        })
    }
}

/// On-link route on the tunnel adapter (NextHop unspecified 0.0.0.0, AF_INET).
fn add_onlink_route(
    luid_value: u64,
    dest: [u8; 4],
    prefix_len: u8,
    metric: u32,
) -> io::Result<MIB_IPFORWARD_ROW2> {
    unsafe {
        let mut row: MIB_IPFORWARD_ROW2 = core::mem::zeroed();
        InitializeIpForwardEntry(&mut row);
        row.InterfaceLuid = NET_LUID_LH { Value: luid_value };
        row.DestinationPrefix.Prefix = sockaddr_v4(dest);
        row.DestinationPrefix.PrefixLength = prefix_len;
        row.NextHop = sockaddr_v4([0, 0, 0, 0]);
        row.Metric = metric;
        row.SitePrefixLength = 0;
        let rc = CreateIpForwardEntry2(&row);
        if rc != NO_ERROR {
            return Err(err(rc));
        }
        Ok(row)
    }
}

/// /32 carve-out through a real gateway on the physical interface.
fn add_gateway_route(
    luid_value: u64,
    dest: [u8; 4],
    prefix_len: u8,
    gateway: [u8; 4],
    metric: u32,
) -> io::Result<MIB_IPFORWARD_ROW2> {
    unsafe {
        let mut row: MIB_IPFORWARD_ROW2 = core::mem::zeroed();
        InitializeIpForwardEntry(&mut row);
        row.InterfaceLuid = NET_LUID_LH { Value: luid_value };
        row.DestinationPrefix.Prefix = sockaddr_v4(dest);
        row.DestinationPrefix.PrefixLength = prefix_len;
        row.NextHop = sockaddr_v4(gateway);
        row.Metric = metric;
        row.SitePrefixLength = 0;
        let rc = CreateIpForwardEntry2(&row);
        if rc != NO_ERROR {
            return Err(err(rc));
        }
        Ok(row)
    }
}
