//! rickynet.exe — the RickyNet Windows client.
//!
//! Default: launches a small egui GUI (PdaNet-style). `--headless` runs the
//! bridge without a window for scripting/testing. See `args::HELP`.
//!
//! The heavy lifting (Wintun I/O, usbmux/Wi-Fi transport, routing, the packet
//! pump) is Windows-only and runs on background threads; the portable modules
//! (`usbmux`, `transport`, `args`, `state`) build and unit-test on any host.

// Off-Windows we only compile the portable subset (for tests), so a lot is
// "unused" there — keep that build warning-clean.
#![cfg_attr(not(windows), allow(dead_code))]
// The GUI subsystem hides the console window on Windows release builds; a
// console is still attached for --headless via AttachConsole in windows_main.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod args;
mod logging;
mod state;
mod transport;
mod usbmux;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod elevation;
#[cfg(windows)]
mod icon;
#[cfg(windows)]
mod routing;
#[cfg(windows)]
mod tun;
#[cfg(windows)]
mod worker;

fn main() {
    // RUST_LOG overrides; defaults to debug for RickyNet code. Logs are teed to
    // stderr AND a file (see `logging`) so the GUI build still produces logs.
    logging::init();

    let parsed = match args::parse(std::env::args()) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{}", args::HELP);
            return;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{}", args::HELP);
            std::process::exit(2);
        }
    };

    #[cfg(windows)]
    windows_main(parsed);

    #[cfg(not(windows))]
    {
        eprintln!(
            "rickynet-win targets Windows (Wintun + win32). Parsed OK: \
             transport={:?} port={} headless={} — build/run this on Windows.",
            parsed.transport, parsed.port, parsed.headless
        );
    }
}

#[cfg(windows)]
fn windows_main(args: args::Args) {
    // The GUI build uses the "windows" subsystem (no console). For --headless,
    // reattach to the launching console so logs/prints are visible.
    if args.headless {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }

    // Primary elevation mechanism is the embedded requireAdministrator manifest
    // (build.rs). This runas relaunch is a fallback if the manifest is stripped.
    let elevated = elevation::is_elevated();
    log::info!(
        "mode: {}, transport: {}, port: {}, elevated: {elevated}",
        if args.headless { "headless" } else { "gui" },
        args.transport.label(),
        args.port
    );
    if !elevated {
        log::warn!("not elevated; attempting UAC relaunch");
        if elevation::relaunch_as_admin() {
            // An elevated instance was spawned; this one exits.
            log::info!("elevated instance launched; this instance exits");
            return;
        }
        log::warn!("UAC relaunch failed or was declined");
        if args.headless {
            eprintln!(
                "RickyNet needs Administrator to create the network adapter and set \
                 routes. Re-run from an elevated prompt."
            );
            std::process::exit(1);
        }
        // GUI: continue, but the app will show the elevation warning and keep
        // the Connect button disabled (never a fake-connected state).
    }

    if args.headless {
        worker::run_headless(&args);
    } else {
        app::run(args, elevated);
    }
}
