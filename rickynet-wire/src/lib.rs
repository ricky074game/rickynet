//! RickyNet wire protocol — the byte-stream framing shared by the Windows client
//! (`rickynet-win`) and the phone data plane (`rickynet-core`).
//!
//! The link between the desktop and the phone is a plain, ordered byte stream
//! (a TCP socket, reached either over usbmux/USB or directly over Wi-Fi). Every
//! IP packet that crosses that stream is length-prefixed so the reader can
//! recover packet boundaries:
//!
//! ```text
//! +----------------------+------------------------------+
//! | u16 length (big-end) |  <length> bytes of IP packet |
//! +----------------------+------------------------------+
//! ```
//!
//! `length` is the size of the IP packet only (it does **not** include the two
//! length bytes). A u16 length caps a single frame at 65535 bytes, which is the
//! maximum size of an IP packet, so the framing can carry anything a TUN device
//! will ever hand us.
//!
//! This crate is deliberately I/O-agnostic. The core primitive is
//! [`FrameDecoder`], a byte accumulator that yields whole packets as they
//! arrive — it works identically behind a synchronous `std::net::TcpStream`
//! (the Windows side) and a `tokio` stream (the phone side). Thin blocking
//! [`read_frame`]/[`write_frame`] helpers are provided for the synchronous path.

use std::io::{self, Read, Write};

/// Length prefix width, in bytes. The prefix is a big-endian `u16`.
pub const LEN_PREFIX: usize = 2;

/// Largest IP packet we will frame (u16 length ceiling).
pub const MAX_FRAME: usize = u16::MAX as usize;

/// Default TCP port the phone data plane listens on and the desktop dials.
///
/// For USB transport the phone binds `127.0.0.1:DEFAULT_PORT` and usbmux tunnels
/// the desktop's loopback connection to it; for Wi-Fi the phone binds
/// `0.0.0.0:DEFAULT_PORT` on its LAN address. Overridable on both ends.
pub const DEFAULT_PORT: u16 = 27600;

/// Suggested MTU for the desktop TUN adapter. Kept below the common 1500 path
/// MTU so re-originated cellular sockets don't have to fragment the payloads we
/// hand them. Documented here so both ends agree.
pub const DEFAULT_MTU: u16 = 1400;

// --- Transport selector (mirrors the C ABI `transport: u32` argument) --------

/// USB transport: desktop reaches the phone's `127.0.0.1` listener via usbmux.
pub const TRANSPORT_USB: u32 = 0;
/// Wi-Fi transport: desktop connects directly to the phone's LAN IP.
pub const TRANSPORT_WIFI: u32 = 1;

/// Encode one IP packet into a framed buffer (`[u16 len][packet]`), appending to
/// `out`. Returns the number of bytes written. Panics only via `debug_assert`
/// if handed an over-length packet; in release an over-length packet is
/// truncated-checked by the caller (a TUN read can never exceed the adapter MTU,
/// which is far below 65535).
#[inline]
pub fn encode_frame(packet: &[u8], out: &mut Vec<u8>) -> usize {
    debug_assert!(packet.len() <= MAX_FRAME, "IP packet exceeds u16 frame length");
    let len = packet.len().min(MAX_FRAME) as u16;
    out.reserve(LEN_PREFIX + len as usize);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&packet[..len as usize]);
    LEN_PREFIX + len as usize
}

/// Convenience: allocate and return a framed copy of `packet`.
#[inline]
pub fn frame(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(LEN_PREFIX + packet.len());
    encode_frame(packet, &mut out);
    out
}

/// Incremental frame decoder. Feed it whatever bytes arrive from the stream via
/// [`push`](FrameDecoder::push) (any chunking — a frame may be split across many
/// reads, or several frames may arrive in one read), then drain complete IP
/// packets with [`next_frame`](FrameDecoder::next_frame).
///
/// The internal buffer is compacted as frames are consumed, so steady-state
/// memory stays bounded by roughly one in-flight frame.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// Read cursor into `buf`; bytes before it have been consumed.
    start: usize,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(DEFAULT_MTU as usize + LEN_PREFIX), start: 0 }
    }

    /// Append received bytes to the decode buffer.
    #[inline]
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Number of unconsumed bytes currently buffered.
    #[inline]
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.start
    }

    /// Pop the next complete IP packet, or `None` if a full frame isn't buffered
    /// yet. The returned `Vec` is the IP packet with the length prefix stripped.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let avail = self.buf.len() - self.start;
        if avail < LEN_PREFIX {
            self.compact();
            return None;
        }
        let len = u16::from_be_bytes([self.buf[self.start], self.buf[self.start + 1]]) as usize;
        if avail < LEN_PREFIX + len {
            self.compact();
            return None;
        }
        let body_start = self.start + LEN_PREFIX;
        let frame = self.buf[body_start..body_start + len].to_vec();
        self.start = body_start + len;
        // If we've drained everything, reset to reuse the allocation cheaply.
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        Some(frame)
    }

    /// Drop already-consumed bytes from the front of the buffer once they add up
    /// to a meaningful amount, to keep the buffer from growing unbounded on a
    /// long-lived connection.
    fn compact(&mut self) {
        if self.start == 0 {
            return;
        }
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start >= 4096 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }
}

/// Blocking read of exactly one frame from `r`. Returns the IP packet bytes.
/// Errors with `UnexpectedEof` if the stream closes mid-frame.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; LEN_PREFIX];
    r.read_exact(&mut len_buf)?;
    let len = u16::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Blocking write of one framed IP packet to `w`. Does not flush.
pub fn write_frame<W: Write>(w: &mut W, packet: &[u8]) -> io::Result<()> {
    if packet.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IP packet exceeds u16 frame length",
        ));
    }
    let len = (packet.len() as u16).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(packet)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_single() {
        let pkt = b"hello ip packet";
        let framed = frame(pkt);
        assert_eq!(&framed[..2], &(pkt.len() as u16).to_be_bytes());
        let mut d = FrameDecoder::new();
        d.push(&framed);
        assert_eq!(d.next_frame().unwrap(), pkt);
        assert!(d.next_frame().is_none());
    }

    #[test]
    fn decoder_handles_split_across_reads() {
        let pkt = vec![7u8; 500];
        let framed = frame(&pkt);
        let mut d = FrameDecoder::new();
        // Feed one byte at a time — the hardest possible chunking.
        for b in &framed {
            assert!(d.next_frame().is_none() || d.buffered() == 0);
            d.push(std::slice::from_ref(b));
        }
        assert_eq!(d.next_frame().unwrap(), pkt);
        assert!(d.next_frame().is_none());
    }

    #[test]
    fn decoder_handles_multiple_frames_in_one_push() {
        let a = vec![1u8; 3];
        let b = vec![2u8; 40];
        let c = vec![3u8; 1200];
        let mut blob = Vec::new();
        encode_frame(&a, &mut blob);
        encode_frame(&b, &mut blob);
        encode_frame(&c, &mut blob);
        let mut d = FrameDecoder::new();
        d.push(&blob);
        assert_eq!(d.next_frame().unwrap(), a);
        assert_eq!(d.next_frame().unwrap(), b);
        assert_eq!(d.next_frame().unwrap(), c);
        assert!(d.next_frame().is_none());
    }

    #[test]
    fn empty_frame_is_valid() {
        let framed = frame(&[]);
        assert_eq!(framed, vec![0, 0]);
        let mut d = FrameDecoder::new();
        d.push(&framed);
        assert_eq!(d.next_frame().unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn blocking_read_write_roundtrip() {
        let pkt = vec![42u8; 1300];
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &pkt).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let got = read_frame(&mut cursor).unwrap();
        assert_eq!(got, pkt);
    }

    #[test]
    fn compaction_keeps_buffer_bounded() {
        let mut d = FrameDecoder::new();
        let pkt = vec![9u8; 100];
        for _ in 0..1000 {
            d.push(&frame(&pkt));
            let got = d.next_frame().unwrap();
            assert_eq!(got.len(), 100);
        }
        // After draining, the buffer must not have accumulated 1000 frames.
        assert!(d.buffered() < 4096, "buffer grew unbounded: {}", d.buffered());
    }
}
