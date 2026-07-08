//! Cellular egress — the one real trick.
//!
//! Every socket we re-originate on the phone must leave over the cellular
//! interface (`pdp_ip0`) even when Wi-Fi is up, otherwise the desktop's traffic
//! would ride the phone's Wi-Fi and the whole point (spend cellular, not Wi-Fi)
//! is lost. On Apple platforms the BSD-level lever for that is `IP_BOUND_IF`
//! (IPv4) / `IPV6_BOUND_IF` (IPv6): set the outbound interface *index* on the
//! socket BEFORE `connect()`, and the kernel scopes route + source-address
//! selection to that interface.
//!
//! Interface discovery: `if_nametoindex("pdp_ip0")` first, then a `getifaddrs()`
//! scan for any interface whose name starts with `pdp_ip` (dual-SIM / different
//! basebands can name it `pdp_ip1`, etc.). If nothing cellular is found (Wi-Fi
//! only device, or the iOS Simulator) we log once and DON'T bind, so the flow
//! still works over the default route — degraded (rides Wi-Fi) but not broken.
//!
//! NOTE (future, more robust path): the officially-blessed approach is to do
//! egress in Swift via `Network.framework`
//! `NWParameters.requiredInterfaceType = .cellular`, create the connection
//! there, and hand the connected fd down to Rust. For v1 the all-Rust
//! `pdp_ip0` + `IP_BOUND_IF` path is fine and self-contained.

use std::io;
use std::net::SocketAddr;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::net::{TcpSocket, TcpStream, UdpSocket};

#[cfg(target_vendor = "apple")]
use std::os::unix::io::{AsRawFd, RawFd};

/// Resolve the cellular interface index, or `None` if no cellular interface is
/// present. Cheap enough to call per flow (a `if_nametoindex` is a couple of
/// syscalls); logging is rate-limited to once per distinct outcome.
#[cfg(target_vendor = "apple")]
pub fn cellular_ifindex() -> Option<u32> {
    use std::ffi::{CStr, CString};
    use std::sync::atomic::{AtomicU8, Ordering};

    // 0 = not yet logged, 1 = logged "found", 2 = logged "missing".
    static LOGGED: AtomicU8 = AtomicU8::new(0);

    // Fast path: the canonical name on single-SIM cellular iPhones.
    if let Ok(name) = CString::new("pdp_ip0") {
        let idx = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if idx != 0 {
            if LOGGED.swap(1, Ordering::Relaxed) != 1 {
                log::info!("cellular egress: bound to pdp_ip0 (ifindex {idx})");
            }
            return Some(idx);
        }
    }

    // Fallback: scan for any pdp_ip* interface.
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return None;
    }
    let mut found: Option<u32> = None;
    let mut cur = head;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        if !ifa.ifa_name.is_null() {
            if let Ok(name) = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_str() {
                if name.starts_with("pdp_ip") {
                    let cname = CString::new(name).ok();
                    let idx = cname
                        .as_ref()
                        .map(|c| unsafe { libc::if_nametoindex(c.as_ptr()) })
                        .unwrap_or(0);
                    if idx != 0 {
                        found = Some(idx);
                        if LOGGED.swap(1, Ordering::Relaxed) != 1 {
                            log::info!("cellular egress: bound to {name} (ifindex {idx})");
                        }
                        break;
                    }
                }
            }
        }
        cur = ifa.ifa_next;
    }
    unsafe { libc::freeifaddrs(head) };

    if found.is_none() && LOGGED.swap(2, Ordering::Relaxed) != 2 {
        log::warn!(
            "cellular egress: no pdp_ip* interface found; re-originated sockets \
             will use the default route (likely Wi-Fi). This is expected on the \
             Simulator or a Wi-Fi-only device."
        );
    }
    found
}

/// Apply `IP_BOUND_IF` / `IPV6_BOUND_IF` to a raw fd. Best-effort: a failure to
/// find cellular is not fatal (we fall through to the default route); a real
/// setsockopt error is logged but also non-fatal so the flow still connects.
#[cfg(target_vendor = "apple")]
fn bind_fd_to_cellular(fd: RawFd, is_v6: bool) {
    let Some(idx) = cellular_ifindex() else {
        return;
    };
    let idx: libc::c_uint = idx;
    let (level, name) = if is_v6 {
        (libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF) // 41, 125
    } else {
        (libc::IPPROTO_IP, libc::IP_BOUND_IF) // 0, 25
    };
    // optval is the raw ifindex in host byte order (NOT htonl), passed by address.
    let ret = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            &idx as *const libc::c_uint as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    if ret != 0 {
        log::warn!(
            "cellular egress: setsockopt(IP_BOUND_IF) failed: {}",
            io::Error::last_os_error()
        );
    }
}

/// Non-Apple builds (host/CI Linux) can't bind to `pdp_ip0`; this is a no-op so
/// the crate compiles and the Wi-Fi transport is testable off-device.
#[cfg(not(target_vendor = "apple"))]
#[allow(dead_code)]
pub fn cellular_ifindex() -> Option<u32> {
    None
}

/// Open a real TCP connection to `dst`, egressing over cellular. Uses
/// `tokio::net::TcpSocket` so we can set `IP_BOUND_IF` on the fd *before*
/// `connect()`, which is when the route is chosen.
pub async fn connect_tcp_cellular(dst: SocketAddr) -> io::Result<TcpStream> {
    let sock = if dst.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    #[cfg(target_vendor = "apple")]
    bind_fd_to_cellular(sock.as_raw_fd(), dst.is_ipv6());
    sock.connect(dst).await
}

/// Create a real UDP socket connected to `dst`, egressing over cellular.
pub async fn connect_udp_cellular(dst: SocketAddr) -> io::Result<UdpSocket> {
    let domain = if dst.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    #[cfg(target_vendor = "apple")]
    bind_fd_to_cellular(sock.as_raw_fd(), dst.is_ipv6());
    sock.set_nonblocking(true)?;
    let bind_addr: SocketAddr = if dst.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    sock.bind(&SockAddr::from(bind_addr))?;
    let std_udp: std::net::UdpSocket = sock.into();
    let udp = UdpSocket::from_std(std_udp)?;
    udp.connect(dst).await?;
    Ok(udp)
}

/// Create an unprivileged ICMP datagram socket (`SOCK_DGRAM`/`IPPROTO_ICMP`),
/// egressing over cellular, for real end-to-end ping re-origination. iOS and
/// macOS permit this without root (the "ping socket"). Returns an unconnected
/// socket to be driven with `send_to`/`recv_from`. `None` if the platform
/// refuses to create it (e.g. Linux without `net.ipv4.ping_group_range`), in
/// which case the caller falls back to answering the echo locally.
pub fn icmp_v4_socket_cellular() -> Option<UdpSocket> {
    let sock = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
        Ok(s) => s,
        Err(e) => {
            log::debug!("ICMP: unprivileged ICMP socket unavailable ({e}); will answer locally");
            return None;
        }
    };
    #[cfg(target_vendor = "apple")]
    bind_fd_to_cellular(sock.as_raw_fd(), false);
    if sock.set_nonblocking(true).is_err() {
        return None;
    }
    let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    if sock.bind(&SockAddr::from(bind_addr)).is_err() {
        return None;
    }
    let std_udp: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_udp).ok()
}
