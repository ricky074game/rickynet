//! Native usbmux client (no external iproxy.exe).
//!
//! On Windows, iTunes / the "Apple Devices" app install the Apple Mobile Device
//! Service, which speaks the usbmux protocol on `127.0.0.1:27015`. This is the
//! same protocol as libusbmuxd. We use it to tunnel a TCP connection to a port
//! the RickyNet iOS app is `listen()`-ing on inside the phone.
//!
//! Wire format (all header fields little-endian):
//! ```text
//!   struct { u32 length; u32 version=1; u32 message=8; u32 tag; } + XML plist
//! ```
//! `length` includes the 16-byte header. `message = 8` = plist message.
//!
//! Handshake:
//!   1. ListDevices -> pick a device whose Properties.ConnectionType == "USB".
//!   2. On a fresh socket, Connect{ DeviceID, PortNumber = htons(port) };
//!      a Result plist with Number == 0 means success and THAT SAME SOCKET
//!      becomes a transparent raw pipe to the device port (no more framing).
//!
//! Source-verified against libimobiledevice usbmuxd-proto.h / libusbmuxd.c.

use std::io::{self, Cursor, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

/// Apple Mobile Device Service loopback port on Windows.
pub const USBMUXD_PORT: u16 = 27015;

const HEADER_LEN: u32 = 16;
const VERSION_PLIST: u32 = 1;
const MSG_PLIST: u32 = 8;

/// A device as reported by ListDevices.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_id: u64,
    pub serial: String,
}

fn plist_err(e: plist::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("plist: {e}"))
}

fn as_u64(v: &plist::Value) -> Option<u64> {
    v.as_unsigned_integer()
        .or_else(|| v.as_signed_integer().and_then(|i| u64::try_from(i).ok()))
}

/// `PortNumber` must be the port in network byte order packed into the integer,
/// i.e. byte-swapped from host order on the (always little-endian) client.
/// e.g. 5555 -> 45845, 22 -> 5632. This is `htons(port)` on a LE machine.
fn port_network_order(port: u16) -> u64 {
    port.swap_bytes() as u64
}

fn base_message(msg_type: &str) -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    d.insert("MessageType".into(), plist::Value::from(msg_type));
    d.insert(
        "ClientVersionString".into(),
        plist::Value::from("rickynet-usbmux 0.1"),
    );
    d.insert("ProgName".into(), plist::Value::from("RickyNet"));
    d.insert("kLibUSBMuxVersion".into(), plist::Value::from(3u64));
    d
}

fn send_plist<W: Write>(w: &mut W, tag: u32, dict: &plist::Dictionary) -> io::Result<()> {
    let mut body = Vec::new();
    plist::Value::Dictionary(dict.clone())
        .to_writer_xml(&mut body)
        .map_err(plist_err)?;
    let length = HEADER_LEN + body.len() as u32;
    w.write_u32::<LittleEndian>(length)?;
    w.write_u32::<LittleEndian>(VERSION_PLIST)?;
    w.write_u32::<LittleEndian>(MSG_PLIST)?;
    w.write_u32::<LittleEndian>(tag)?;
    w.write_all(&body)?;
    w.flush()
}

fn recv_plist<R: Read>(r: &mut R) -> io::Result<plist::Value> {
    let mut hdr = [0u8; 16];
    r.read_exact(&mut hdr)?;
    let mut c = Cursor::new(&hdr[..]);
    let length = c.read_u32::<LittleEndian>()?;
    let _version = c.read_u32::<LittleEndian>()?;
    let _message = c.read_u32::<LittleEndian>()?;
    let _tag = c.read_u32::<LittleEndian>()?;
    if length < HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "usbmux: short frame length",
        ));
    }
    let body_len = (length - HEADER_LEN) as usize;
    let mut body = vec![0u8; body_len];
    r.read_exact(&mut body)?;
    // from_reader auto-detects XML vs binary plist (Cursor is Read + Seek).
    plist::Value::from_reader(Cursor::new(body)).map_err(plist_err)
}

fn connect_muxd() -> io::Result<TcpStream> {
    log::debug!("usbmux: connecting to Apple Mobile Device Service at 127.0.0.1:{USBMUXD_PORT}");
    let s = TcpStream::connect(("127.0.0.1", USBMUXD_PORT)).map_err(|e| {
        log::error!(
            "usbmux: cannot reach 127.0.0.1:{USBMUXD_PORT}: {e} — is iTunes / Apple \
             Devices installed and the Apple Mobile Device Service running?"
        );
        e
    })?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(s)
}

/// Query connected USB devices.
pub fn list_devices() -> io::Result<Vec<DeviceInfo>> {
    let mut s = connect_muxd()?;
    log::debug!("usbmux: sending ListDevices");
    send_plist(&mut s, 1, &base_message("ListDevices"))?;
    let resp = recv_plist(&mut s)?;
    let devices = parse_device_list(&resp);
    log::info!("usbmux: {} USB device(s) attached", devices.len());
    for d in &devices {
        log::debug!("usbmux:   device id {} serial {}", d.device_id, d.serial);
    }
    Ok(devices)
}

fn parse_device_list(resp: &plist::Value) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    let Some(list) = resp
        .as_dictionary()
        .and_then(|d| d.get("DeviceList"))
        .and_then(|v| v.as_array())
    else {
        return out;
    };
    for item in list {
        let Some(props) = item
            .as_dictionary()
            .and_then(|d| d.get("Properties"))
            .and_then(|v| v.as_dictionary())
        else {
            continue;
        };
        let conn = props
            .get("ConnectionType")
            .and_then(|v| v.as_string())
            .unwrap_or("");
        if conn != "USB" {
            continue;
        }
        let Some(device_id) = props.get("DeviceID").and_then(as_u64) else {
            continue;
        };
        let serial = props
            .get("SerialNumber")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_string();
        out.push(DeviceInfo { device_id, serial });
    }
    out
}

/// Open a tunneled connection to `port` on `device_id`. On success the returned
/// stream is a raw pipe to the device port (usbmux framing is done).
pub fn connect_device(device_id: u64, port: u16) -> io::Result<TcpStream> {
    let mut s = connect_muxd()?;
    log::debug!(
        "usbmux: Connect device {device_id} port {port} (network-order {})",
        port_network_order(port)
    );
    let mut d = base_message("Connect");
    d.insert("DeviceID".into(), plist::Value::from(device_id));
    d.insert(
        "PortNumber".into(),
        plist::Value::from(port_network_order(port)),
    );
    send_plist(&mut s, 2, &d)?;
    let resp = recv_plist(&mut s)?;
    let number = resp
        .as_dictionary()
        .and_then(|d| d.get("Number"))
        .and_then(as_u64)
        .unwrap_or(u64::MAX);
    log::debug!("usbmux: Connect result Number={number}");
    if number != 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!(
                "usbmux Connect failed (Number={number}; 3=connection refused — is the \
                 RickyNet app open and Started on the phone?)"
            ),
        ));
    }
    // Hand back a blocking raw pipe.
    s.set_read_timeout(None)?;
    s.set_write_timeout(None)?;
    log::info!("usbmux: tunnel to device {device_id} port {port} established");
    Ok(s)
}

/// Convenience: pick the first USB device and tunnel to `port`.
pub fn connect_first_device(port: u16) -> io::Result<TcpStream> {
    let devices = list_devices()?;
    let dev = devices.into_iter().next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no USB iPhone found via usbmux — plug in the phone, tap Trust, and make \
             sure iTunes / Apple Devices (Apple Mobile Device Service) is installed",
        )
    })?;
    log::info!("usbmux: connecting to device {} ({})", dev.device_id, dev.serial);
    connect_device(dev.device_id, port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_swap_matches_htons() {
        assert_eq!(port_network_order(5555), 45845);
        assert_eq!(port_network_order(22), 5632);
        assert_eq!(port_network_order(62078), 32498);
    }

    #[test]
    fn header_is_little_endian_and_well_formed() {
        let mut buf = Vec::new();
        send_plist(&mut buf, 7, &base_message("ListDevices")).unwrap();
        // length (LE) = total; version=1; message=8; tag=7.
        let length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let version = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let message = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let tag = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        assert_eq!(length as usize, buf.len());
        assert_eq!(version, 1);
        assert_eq!(message, 8);
        assert_eq!(tag, 7);
        // body is an XML plist mentioning our MessageType
        let body = String::from_utf8_lossy(&buf[16..]);
        assert!(body.contains("ListDevices"), "body: {body}");
    }

    #[test]
    fn roundtrip_send_then_recv() {
        // Encode a Result plist the way the daemon would, then parse it back.
        let mut d = plist::Dictionary::new();
        d.insert("MessageType".into(), plist::Value::from("Result"));
        d.insert("Number".into(), plist::Value::from(0u64));
        let mut buf = Vec::new();
        send_plist(&mut buf, 2, &d).unwrap();
        let mut cur = Cursor::new(buf);
        let val = recv_plist(&mut cur).unwrap();
        let num = as_u64(val.as_dictionary().unwrap().get("Number").unwrap()).unwrap();
        assert_eq!(num, 0);
    }

    #[test]
    fn parse_device_list_filters_usb() {
        // Build a DeviceList with one USB and one Network device.
        let mk = |id: u64, conn: &str| {
            let mut props = plist::Dictionary::new();
            props.insert("DeviceID".into(), plist::Value::from(id));
            props.insert("ConnectionType".into(), plist::Value::from(conn));
            props.insert("SerialNumber".into(), plist::Value::from(format!("SER{id}")));
            let mut item = plist::Dictionary::new();
            item.insert("DeviceID".into(), plist::Value::from(id));
            item.insert("MessageType".into(), plist::Value::from("Attached"));
            item.insert("Properties".into(), plist::Value::Dictionary(props));
            plist::Value::Dictionary(item)
        };
        let mut root = plist::Dictionary::new();
        root.insert(
            "DeviceList".into(),
            plist::Value::Array(vec![mk(11, "USB"), mk(22, "Network")]),
        );
        let devices = parse_device_list(&plist::Value::Dictionary(root));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_id, 11);
        assert_eq!(devices[0].serial, "SER11");
    }
}
