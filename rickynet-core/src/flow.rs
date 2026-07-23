//! Per-flow handlers: each flow that `ipstack` terminates is re-originated as a
//! real OS socket bound to cellular, then spliced back to the netstack socket.
//!
//! Byte counting happens at the cable in `device.rs` (upload=TX, download=RX),
//! so these handlers just move bytes and tear down cleanly. Both directions are
//! raced with `select!`/`copy_bidirectional` so an idle or half-open flow is
//! reaped (and its cellular fd freed) rather than leaked — important for UDP,
//! which has no FIN, so a naive `join!` would wait forever after DNS replies.
//!
//! Every flow gets a monotonically-increasing id (`TCP#7`, `UDP#12`) so its
//! open/close/error lines can be correlated in the log.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use etherparse::{Icmpv4Header, Icmpv4Type};
use ipstack::{IpNumber, IpStackTcpStream, IpStackUdpStream, IpStackUnknownTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::egress;

const ICMP_TIMEOUT: Duration = Duration::from_secs(2);

static NEXT_FLOW_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_FLOW_ID.fetch_add(1, Ordering::Relaxed)
}

/// A new TCP flow: dial the original destination over cellular, splice.
pub async fn handle_tcp(tcp: IpStackTcpStream) {
    let id = next_id();
    // For a TUN-intercepted flow, `peer_addr` is the address the desktop tried
    // to reach — i.e. the original destination we must re-originate to.
    let dst = tcp.peer_addr();
    let src = tcp.local_addr();
    log::debug!("TCP#{id}: open {src} -> {dst}, dialing over cellular");
    let t0 = Instant::now();
    let mut real = match egress::connect_tcp_cellular(dst).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!(
                "TCP#{id} {dst}: cellular connect failed after {:.1?}: {e} (kind {:?})",
                t0.elapsed(),
                e.kind()
            );
            return;
        }
    };
    log::debug!(
        "TCP#{id} {dst}: cellular connected in {:.1?} (local {:?})",
        t0.elapsed(),
        real.local_addr().ok()
    );
    let mut tcp = tcp;
    // copy_bidirectional handles half-close correctly AND propagates an error
    // (e.g. ipstack's idle timeout) by returning, so nothing is leaked.
    match tokio::io::copy_bidirectional(&mut tcp, &mut real).await {
        Ok((up, down)) => log::debug!(
            "TCP#{id} {dst}: closed after {:.1?} (up {up} B, down {down} B)",
            t0.elapsed()
        ),
        Err(e) => log::debug!(
            "TCP#{id} {dst}: relay ended after {:.1?}: {e} (kind {:?})",
            t0.elapsed(),
            e.kind()
        ),
    }
}

/// A new UDP flow (includes DNS on :53): one real cellular UDP socket per flow,
/// datagram-preserving relay in both directions, torn down when either side ends.
pub async fn handle_udp(udp: IpStackUdpStream) {
    let id = next_id();
    let dst = udp.peer_addr();
    let src = udp.local_addr();
    log::debug!("UDP#{id}: open {src} -> {dst}");
    let t0 = Instant::now();
    let real = match egress::connect_udp_cellular(dst).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("UDP#{id} {dst}: cellular socket failed: {e} (kind {:?})", e.kind());
            return;
        }
    };
    let real = Arc::new(real);
    let real_up = real.clone();
    let (mut udp_r, mut udp_w) = tokio::io::split(udp);

    let up_bytes = Arc::new(AtomicU64::new(0));
    let down_bytes = Arc::new(AtomicU64::new(0));
    let (up_ctr, down_ctr) = (up_bytes.clone(), down_bytes.clone());

    // desktop -> internet
    let up = async move {
        let mut buf = vec![0u8; 65535];
        loop {
            let n = match udp_r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if real_up.send(&buf[..n]).await.is_err() {
                break;
            }
            up_ctr.fetch_add(n as u64, Ordering::Relaxed);
        }
    };
    // internet -> desktop
    let down = async move {
        let mut buf = vec![0u8; 65535];
        loop {
            let n = match real.recv(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            if udp_w.write_all(&buf[..n]).await.is_err() {
                break;
            }
            down_ctr.fetch_add(n as u64, Ordering::Relaxed);
        }
    };

    // Whichever side ends first (ipstack's UDP idle timeout fires `up`) tears
    // the flow down and drops the cellular socket — no fd leak.
    let ended_by = tokio::select! {
        _ = up => "desktop side",
        _ = down => "internet side",
    };
    log::debug!(
        "UDP#{id} {dst}: closed by {ended_by} after {:.1?} (up {} B, down {} B)",
        t0.elapsed(),
        up_bytes.load(Ordering::Relaxed),
        down_bytes.load(Ordering::Relaxed)
    );
}

/// A non-TCP/UDP flow. v1 handles IPv4 ICMP echo (ping): re-originate the echo
/// over cellular via an unprivileged ICMP datagram socket; if the real host
/// answers, hand the desktop an echo reply. Everything else is dropped.
pub async fn handle_unknown(u: IpStackUnknownTransport) {
    if u.ip_protocol() != IpNumber::ICMP || !u.src_addr().is_ipv4() {
        log::debug!("drop unknown transport proto {:?}", u.ip_protocol());
        return;
    }
    let dst = match u.dst_addr() {
        IpAddr::V4(d) => d,
        IpAddr::V6(_) => return,
    };
    let (hdr, data) = match Icmpv4Header::from_slice(u.payload()) {
        Ok(x) => x,
        Err(e) => {
            log::debug!("ICMP: unparseable ICMPv4 header from desktop: {e:?}");
            return;
        }
    };
    let echo = match hdr.icmp_type {
        Icmpv4Type::EchoRequest(e) => e,
        other => {
            log::debug!("ICMP: ignoring non-echo type {other:?}");
            return;
        }
    };
    let echo_bytes = u.payload().to_vec();
    let data = data.to_vec();
    log::debug!("ICMP: echo request for {dst} ({} payload bytes)", data.len());

    // Try a genuine round-trip over cellular.
    if let Some(sock) = egress::icmp_v4_socket_cellular() {
        let target = SocketAddr::new(IpAddr::V4(dst), 0);
        if sock.send_to(&echo_bytes, target).await.is_ok() {
            let mut rbuf = vec![0u8; 2048];
            match tokio::time::timeout(ICMP_TIMEOUT, sock.recv_from(&mut rbuf)).await {
                Ok(Ok((n, _))) if n > 0 => {
                    log::debug!("ICMP: {dst} answered ({n} bytes); relaying echo reply");
                    reply_to_desktop(&u, echo, &data);
                }
                // No reply from the real host within the timeout: let the
                // desktop's ping time out naturally (honest — host unreachable).
                _ => {
                    log::debug!("ICMP: no reply from {dst} within {ICMP_TIMEOUT:?}");
                }
            }
        } else {
            log::debug!("ICMP: send_to {dst} failed");
        }
        return;
    }

    // No unprivileged ICMP socket on this platform (e.g. a hardened Linux host
    // in CI): answer locally so a tunnel-liveness ping still succeeds.
    log::debug!("ICMP: answering echo for {dst} locally (no ICMP socket)");
    reply_to_desktop(&u, echo, &data);
}

fn reply_to_desktop(u: &IpStackUnknownTransport, echo: etherparse::IcmpEchoHeader, data: &[u8]) {
    let mut resp = Icmpv4Header::new(Icmpv4Type::EchoReply(echo));
    resp.update_checksum(data);
    let mut payload = resp.to_bytes().to_vec();
    payload.extend_from_slice(data);
    if let Err(e) = u.send(payload) {
        log::debug!("ICMP: reply to desktop failed: {e}");
    }
}
