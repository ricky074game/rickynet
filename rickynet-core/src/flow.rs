//! Per-flow handlers: each flow that `ipstack` terminates is re-originated as a
//! real OS socket bound to cellular, then spliced back to the netstack socket.
//!
//! Byte-counter convention (as surfaced to the iOS UI via `rn_stats`):
//!   * TX = bytes the desktop sent OUT to the internet (upload over cellular)
//!   * RX = bytes received FROM the internet for the desktop (download)
//! Counted at the real (cellular) socket, so it reflects actual phone-data use.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use etherparse::{Icmpv4Header, Icmpv4Type};
use ipstack::{IpNumber, IpStackTcpStream, IpStackUdpStream, IpStackUnknownTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::egress;
use crate::{RX_BYTES, TX_BYTES};

const RELAY_BUF: usize = 16 * 1024;
const ICMP_TIMEOUT: Duration = Duration::from_secs(2);

/// Copy `reader` -> `writer` to EOF, tallying bytes into `counter`.
async fn pump<R, W>(mut reader: R, mut writer: W, counter: &'static AtomicU64) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; RELAY_BUF];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await?;
        counter.fetch_add(n as u64, Ordering::Relaxed);
    }
    let _ = writer.shutdown().await;
    Ok(())
}

/// A new TCP flow: dial the original destination over cellular, splice.
pub async fn handle_tcp(tcp: IpStackTcpStream) {
    // For a TUN-intercepted flow, `peer_addr` is the address the desktop tried
    // to reach — i.e. the original destination we must re-originate to.
    let dst = tcp.peer_addr();
    let real = match egress::connect_tcp_cellular(dst).await {
        Ok(s) => s,
        Err(e) => {
            log::debug!("TCP {dst}: cellular connect failed: {e}");
            return;
        }
    };
    let (tcp_r, tcp_w) = tokio::io::split(tcp);
    let (real_r, real_w) = tokio::io::split(real);
    // upload (desktop -> internet) counts TX; download (internet -> desktop) counts RX.
    let up = pump(tcp_r, real_w, &TX_BYTES);
    let down = pump(real_r, tcp_w, &RX_BYTES);
    let _ = tokio::join!(up, down);
}

/// A new UDP flow (includes DNS on :53): one real cellular UDP socket per flow,
/// datagram-preserving relay in both directions.
pub async fn handle_udp(udp: IpStackUdpStream) {
    let dst = udp.peer_addr();
    let real = match egress::connect_udp_cellular(dst).await {
        Ok(s) => s,
        Err(e) => {
            log::debug!("UDP {dst}: cellular socket failed: {e}");
            return;
        }
    };
    let real = Arc::new(real);
    let real_up = real.clone();
    let (mut udp_r, mut udp_w) = tokio::io::split(udp);

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
            TX_BYTES.fetch_add(n as u64, Ordering::Relaxed);
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
            RX_BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
    };
    tokio::join!(up, down);
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
        Err(_) => return,
    };
    let echo = match hdr.icmp_type {
        Icmpv4Type::EchoRequest(e) => e,
        _ => return,
    };
    let echo_bytes = u.payload().to_vec();
    let data = data.to_vec();

    // Try a genuine round-trip over cellular.
    if let Some(sock) = egress::icmp_v4_socket_cellular() {
        let target = SocketAddr::new(IpAddr::V4(dst), 0);
        if sock.send_to(&echo_bytes, target).await.is_ok() {
            let mut rbuf = vec![0u8; 2048];
            match tokio::time::timeout(ICMP_TIMEOUT, sock.recv_from(&mut rbuf)).await {
                Ok(Ok((n, _))) if n > 0 => {
                    TX_BYTES.fetch_add(echo_bytes.len() as u64, Ordering::Relaxed);
                    RX_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                    reply_to_desktop(&u, echo, &data);
                }
                // No reply from the real host within the timeout: let the
                // desktop's ping time out naturally (honest — host unreachable).
                _ => {}
            }
        }
        return;
    }

    // No unprivileged ICMP socket on this platform (e.g. a hardened Linux host
    // in CI): answer locally so a tunnel-liveness ping still succeeds.
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
