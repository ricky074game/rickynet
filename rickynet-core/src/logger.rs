//! Core logging pipeline: every `log::` record (ours AND dependencies like
//! ipstack/tokio) is formatted into one line and
//!   1. kept in an in-memory ring buffer (last `RING_CAP` lines), and
//!   2. forwarded to the C callback registered via `rn_log_set_callback`, so the
//!      Swift shell can show a live log view and persist lines to a file.
//!
//! When a callback is registered the ring buffer is replayed to it first, so
//! lines logged before registration (e.g. from an early `rn_start`) are not lost.
//!
//! The callback is invoked from arbitrary Rust threads (tokio workers). The
//! Swift side must therefore be thread-safe and MUST NOT call back into any
//! `rn_*` function from inside the callback.

use std::collections::VecDeque;
use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::{Mutex, Once, OnceLock};
use std::time::Instant;

/// C signature of the log sink: receives one NUL-terminated UTF-8 line. The
/// pointer is only valid for the duration of the call — copy, don't keep.
pub type RnLogCallback = extern "C" fn(line: *const c_char);

const RING_CAP: usize = 1000;

struct LogState {
    callback: Option<RnLogCallback>,
    ring: VecDeque<String>,
}

static STATE: Mutex<LogState> = Mutex::new(LogState {
    callback: None,
    ring: VecDeque::new(),
});

static START: OnceLock<Instant> = OnceLock::new();

struct CoreLogger;

static CORE_LOGGER: CoreLogger = CoreLogger;

impl log::Log for CoreLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true // level filtering is done by log::set_max_level
    }

    fn log(&self, record: &log::Record) {
        let elapsed = START.get_or_init(Instant::now).elapsed();
        let line = format!(
            "[{:>9.3}s {:5} {}] {}",
            elapsed.as_secs_f64(),
            record.level(),
            record.target(),
            record.args()
        );

        // Push to the ring and snapshot the callback under the lock, but call
        // the callback OUTSIDE the lock so a slow sink can't stall logging.
        let cb = {
            let mut st = STATE.lock().unwrap_or_else(|p| p.into_inner());
            if st.ring.len() >= RING_CAP {
                st.ring.pop_front();
            }
            st.ring.push_back(line.clone());
            st.callback
        };
        if let Some(cb) = cb {
            emit(cb, &line);
        }
    }

    fn flush(&self) {}
}

fn emit(cb: RnLogCallback, line: &str) {
    // A NUL byte in the message would make CString fail; sanitize instead of
    // dropping the line.
    let c = CString::new(line)
        .unwrap_or_else(|_| CString::new(line.replace('\0', "\\0")).unwrap());
    cb(c.as_ptr());
}

/// Install the logger (idempotent). Defaults to `Debug` so the friend-facing
/// build is chatty; `rn_log_set_level` can dial it up/down.
pub fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        START.get_or_init(Instant::now);
        let _ = log::set_logger(&CORE_LOGGER);
        log::set_max_level(log::LevelFilter::Debug);
        log::info!(
            "rickynet-core v{} logging up (level {})",
            env!("CARGO_PKG_VERSION"),
            log::max_level()
        );
    });
}

/// Register (or clear, with `None`) the line sink, replaying the buffered
/// backlog to a newly-registered callback.
pub fn set_callback(cb: Option<RnLogCallback>) {
    let backlog: Vec<String> = {
        let mut st = STATE.lock().unwrap_or_else(|p| p.into_inner());
        st.callback = cb;
        if cb.is_some() {
            st.ring.iter().cloned().collect()
        } else {
            Vec::new()
        }
    };
    if let Some(cb) = cb {
        for line in &backlog {
            emit(cb, line);
        }
        log::info!("log callback registered ({} buffered lines replayed)", backlog.len());
    }
}
