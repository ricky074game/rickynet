//! Logging setup: env_logger formatting, teed to BOTH stderr and a persistent
//! log file, so logs survive the GUI build (windows subsystem = no console) and
//! can be sent back by a remote tester.
//!
//! File location, first writable wins:
//!   1. `%LOCALAPPDATA%\RickyNet\rickynet.log`
//!   2. next to `rickynet.exe`
//!   3. the system temp dir
//!
//! The chosen path is exposed via `log_file_path()` so the GUI can display it
//! and offer an "Open log" button. The file rotates to `rickynet.old.log` at
//! ~5 MB so what the tester sends back stays manageable.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Where the log file ended up, if a writable location was found.
pub fn log_file_path() -> Option<&'static PathBuf> {
    LOG_PATH.get().and_then(|o| o.as_ref())
}

/// Forwards every formatted record to stderr AND the log file, best-effort on
/// both (a full disk or detached console must never break the bridge).
struct Tee {
    file: Option<File>,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Some(f) = &mut self.file {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        if let Some(f) = &mut self.file {
            let _ = f.flush();
        }
        Ok(())
    }
}

fn open_log_file() -> (Option<PathBuf>, Option<File>) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(base) = std::env::var("LOCALAPPDATA") {
        candidates.push(PathBuf::from(base).join("RickyNet"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
        }
    }
    candidates.push(std::env::temp_dir());

    for dir in candidates {
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let path = dir.join("rickynet.log");
        if let Ok(md) = std::fs::metadata(&path) {
            if md.len() > 5_000_000 {
                let _ = std::fs::rename(&path, dir.join("rickynet.old.log"));
            }
        }
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
            return (Some(path), Some(f));
        }
    }
    (None, None)
}

/// Install the global logger. Default filter is debug for the RickyNet crates
/// and info for dependencies; `RUST_LOG` overrides everything.
pub fn init() {
    let (path, file) = open_log_file();
    let _ = LOG_PATH.set(path);

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,rickynet_win=debug,rickynet=debug,rickynet_core=debug,rickynet_wire=debug",
    ))
    .target(env_logger::Target::Pipe(Box::new(Tee { file })))
    .write_style(env_logger::WriteStyle::Never)
    .init();

    log::info!(
        "=== RickyNet v{} starting (pid {}) ===",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    log::info!("cmdline: {:?}", std::env::args().collect::<Vec<_>>());
    match log_file_path() {
        Some(p) => log::info!("log file: {}", p.display()),
        None => log::warn!("no writable log file location found; logging to stderr only"),
    }
}
