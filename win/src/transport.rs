//! Transport selection: the desktop <-> phone link is always a TCP byte stream
//! carrying `[u16 len][IP packet]`. Two ways to reach the phone's listener:
//!
//!   * USB  — via Apple's usbmux service (native client in `usbmux`).
//!   * Wi-Fi — a direct TCP connection to the phone's LAN IP (no Apple service;
//!             handy for development without the iTunes dependency chain).
//!
//! Both yield a plain `std::net::TcpStream`, so everything downstream (the
//! framed packet pump) is transport-agnostic.

use std::io;
use std::net::TcpStream;

use crate::usbmux;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Usb,
    Wifi,
}

impl TransportKind {
    pub fn label(self) -> &'static str {
        match self {
            TransportKind::Usb => "USB",
            TransportKind::Wifi => "Wi-Fi",
        }
    }
}

/// Establish the phone link. `phone_ip` is required for Wi-Fi, ignored for USB.
pub fn connect(kind: TransportKind, phone_ip: Option<&str>, port: u16) -> io::Result<TcpStream> {
    match kind {
        TransportKind::Usb => usbmux::connect_first_device(port),
        TransportKind::Wifi => {
            let ip = phone_ip.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Wi-Fi transport requires --phone-ip",
                )
            })?;
            let stream = TcpStream::connect((ip, port))?;
            stream.set_nodelay(true).ok();
            // The pump blocks on reads; make sure no timeout is inherited.
            stream.set_read_timeout(None).ok();
            Ok(stream)
        }
    }
}
