//! Tiny hand-rolled CLI parser (no clap dependency).
//!
//!   rickynet.exe                              -> launch the GUI (default)
//!   rickynet.exe --headless --transport usb   -> headless bridge for scripting
//!   rickynet.exe --transport wifi --phone-ip 192.168.1.42 [--port 27600]

use crate::transport::TransportKind;
use rickynet_wire::DEFAULT_PORT;

#[derive(Debug, Clone)]
pub struct Args {
    pub transport: TransportKind,
    pub phone_ip: Option<String>,
    pub port: u16,
    pub headless: bool,
    pub adapter_name: String,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            transport: TransportKind::Usb,
            phone_ip: None,
            port: DEFAULT_PORT,
            headless: false,
            adapter_name: "RickyNet".to_string(),
        }
    }
}

pub const HELP: &str = "\
RickyNet — use an iPhone's cellular data over USB (or Wi-Fi) without Personal Hotspot.

USAGE:
    rickynet [OPTIONS]

OPTIONS:
    --transport <usb|wifi>   Link to the phone (default: usb)
    --phone-ip <IP>          Phone LAN IP (required for --transport wifi)
    --port <PORT>            Phone listener port (default: 27600)
    --headless               Run the bridge without the GUI (for scripting)
    --adapter-name <NAME>    Wintun adapter name (default: RickyNet)
    -h, --help               Show this help

With no options, RickyNet launches its GUI. It requires Administrator to create
the network adapter and set routes (it will prompt for elevation).";

/// Parse `std::env::args()`. Returns `Err(message)` on bad input, `Ok(None)` if
/// help was requested (caller prints HELP and exits 0).
pub fn parse<I: Iterator<Item = String>>(mut it: I) -> Result<Option<Args>, String> {
    let mut args = Args::default();
    let _exe = it.next();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--headless" => args.headless = true,
            "--transport" => {
                let v = it.next().ok_or("--transport needs a value (usb|wifi)")?;
                args.transport = match v.to_ascii_lowercase().as_str() {
                    "usb" => TransportKind::Usb,
                    "wifi" | "wi-fi" => TransportKind::Wifi,
                    other => return Err(format!("unknown transport '{other}' (use usb|wifi)")),
                };
            }
            "--phone-ip" => {
                args.phone_ip = Some(it.next().ok_or("--phone-ip needs a value")?);
            }
            "--port" => {
                let v = it.next().ok_or("--port needs a value")?;
                args.port = v.parse().map_err(|_| format!("invalid port '{v}'"))?;
            }
            "--adapter-name" => {
                args.adapter_name = it.next().ok_or("--adapter-name needs a value")?;
            }
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
    }
    if args.transport == TransportKind::Wifi && args.phone_ip.is_none() {
        return Err("--transport wifi requires --phone-ip <IP>".to_string());
    }
    Ok(Some(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        std::iter::once("rickynet".to_string())
            .chain(a.iter().map(|s| s.to_string()))
            .collect()
    }

    #[test]
    fn defaults_to_gui_usb() {
        let a = parse(v(&[]).into_iter()).unwrap().unwrap();
        assert_eq!(a.transport, TransportKind::Usb);
        assert!(!a.headless);
        assert_eq!(a.port, DEFAULT_PORT);
    }

    #[test]
    fn wifi_requires_phone_ip() {
        assert!(parse(v(&["--transport", "wifi"]).into_iter()).is_err());
        let a = parse(v(&["--transport", "wifi", "--phone-ip", "10.0.0.5"]).into_iter())
            .unwrap()
            .unwrap();
        assert_eq!(a.transport, TransportKind::Wifi);
        assert_eq!(a.phone_ip.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn help_returns_none() {
        assert!(parse(v(&["--help"]).into_iter()).unwrap().is_none());
    }

    #[test]
    fn headless_and_port() {
        let a = parse(v(&["--headless", "--port", "9000"]).into_iter())
            .unwrap()
            .unwrap();
        assert!(a.headless);
        assert_eq!(a.port, 9000);
    }
}
