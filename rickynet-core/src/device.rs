//! `FramedDevice` — the bridge between the byte-stream transport (the cable /
//! Wi-Fi socket carrying `[u16 len][IP packet]`) and `ipstack`, which wants a
//! packet-oriented device that implements tokio `AsyncRead`/`AsyncWrite` (one
//! whole IP packet per read, one per write — exactly like a TUN fd).
//!
//! We split the accepted TCP stream into halves and run a reader task and a
//! writer task that own the framing; `FramedDevice` itself is just the pair of
//! channel endpoints ipstack polls. Byte counters are tallied here at the cable
//! (reader = desktop upload = TX, writer = desktop download = RX); this is the
//! meaningful "cellular data moved" figure and keeps the per-flow handlers free
//! of counting.
//!
//! Disconnect handling (important): ipstack 1.0's device loop has no EOF branch
//! — if `poll_read` reported EOF (`Ok` with 0 bytes) it would busy-loop at 100%
//! CPU forever. So on peer disconnect we (a) return `Pending` from `poll_read`
//! (ipstack's internal read task simply parks) and (b) fire a `oneshot` signal
//! that `bridge()` selects on to drop the whole `IpStack` and reconnect.

use std::io;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};

use crate::{RX_BYTES, TX_BYTES};

/// Backpressure bound for packets read off the cable and awaiting injection.
const INCOMING_CAP: usize = 2048;

pub struct FramedDevice {
    incoming: mpsc::Receiver<Vec<u8>>,
    /// A packet partially copied out to a too-small `ReadBuf` (defensive; with
    /// ipstack's MTU set to u16::MAX this path is never hit in practice).
    leftover: Option<(Vec<u8>, usize)>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
}

impl FramedDevice {
    /// Wrap an accepted transport stream: spawn the reader/writer framing tasks
    /// and return the device ipstack will drive, plus a disconnect receiver that
    /// resolves when the peer goes away (the reader task ends and drops the
    /// paired sender).
    pub fn new(stream: tokio::net::TcpStream) -> (Self, oneshot::Receiver<()>) {
        let _ = stream.set_nodelay(true);
        let (rd, wr) = stream.into_split();
        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(INCOMING_CAP);
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (disc_tx, disc_rx) = oneshot::channel::<()>();

        tokio::spawn(reader_task(rd, in_tx, disc_tx));
        tokio::spawn(writer_task(wr, out_rx));

        (
            FramedDevice {
                incoming: in_rx,
                leftover: None,
                outgoing: out_tx,
            },
            disc_rx,
        )
    }
}

async fn reader_task(mut rd: OwnedReadHalf, tx: mpsc::Sender<Vec<u8>>, _disc: oneshot::Sender<()>) {
    // When this task returns (EOF/error/device dropped), `_disc` drops and the
    // disconnect receiver in bridge() resolves.
    loop {
        // Framing: [u16 big-endian length][length bytes]. tokio's read_u16 is
        // big-endian, matching rickynet-wire.
        let len = match rd.read_u16().await {
            Ok(n) => n as usize,
            Err(_) => break, // EOF or error: desktop went away.
        };
        let mut buf = vec![0u8; len];
        if rd.read_exact(&mut buf).await.is_err() {
            break;
        }
        // Bytes the desktop sent out to the internet = upload = TX.
        TX_BYTES.fetch_add(buf.len() as u64, Ordering::Relaxed);
        if tx.send(buf).await.is_err() {
            break; // device dropped
        }
    }
}

async fn writer_task(mut wr: OwnedWriteHalf, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(pkt) = rx.recv().await {
        if pkt.len() > u16::MAX as usize {
            // Can't frame it; drop rather than corrupt the stream.
            log::warn!("device: dropping oversize outbound packet ({} bytes)", pkt.len());
            continue;
        }
        if wr.write_u16(pkt.len() as u16).await.is_err() {
            break;
        }
        if wr.write_all(&pkt).await.is_err() {
            break;
        }
        // Bytes delivered back to the desktop = download = RX.
        RX_BYTES.fetch_add(pkt.len() as u64, Ordering::Relaxed);
        // Coalesce: only flush when the queue has momentarily drained.
        if rx.is_empty() {
            if wr.flush().await.is_err() {
                break;
            }
        }
    }
}

impl AsyncRead for FramedDevice {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Finish draining a leftover packet first.
        if let Some((pkt, pos)) = self.leftover.take() {
            let n = (pkt.len() - pos).min(buf.remaining());
            buf.put_slice(&pkt[pos..pos + n]);
            if pos + n < pkt.len() {
                self.leftover = Some((pkt, pos + n));
            }
            return Poll::Ready(Ok(()));
        }
        match self.incoming.poll_recv(cx) {
            Poll::Ready(Some(pkt)) => {
                let n = pkt.len().min(buf.remaining());
                buf.put_slice(&pkt[..n]);
                if n < pkt.len() {
                    self.leftover = Some((pkt, n));
                }
                Poll::Ready(Ok(()))
            }
            // Channel closed (peer disconnected): DON'T report EOF — ipstack 1.0
            // has no EOF branch and would busy-loop. Park instead; bridge()
            // tears the whole stack down via the disconnect signal.
            Poll::Ready(None) => Poll::Pending,
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for FramedDevice {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // ipstack hands us exactly one IP packet per call. Queue it for the
        // writer task (unbounded -> never blocks the stack).
        match self.outgoing.send(buf.to_vec()) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "transport writer gone",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
