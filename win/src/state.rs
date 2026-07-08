//! Shared state between the GUI thread and the background worker.
//!
//! The UI thread only ever *reads* this (plus flipping `stop`); the worker
//! writes state/counters/log. Everything is behind atomics or short-lived mutex
//! locks so the UI never blocks during connect/disconnect.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

const LOG_CAP: usize = 8;

pub struct Shared {
    state: Mutex<ConnState>,
    error: Mutex<String>,
    log: Mutex<VecDeque<String>>,
    pub rx: AtomicU64,
    pub tx: AtomicU64,
    /// Set by the UI to ask the worker to tear down.
    stop: AtomicBool,
    /// Whether this process has Administrator rights.
    admin: AtomicBool,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Shared {
            state: Mutex::new(ConnState::Disconnected),
            error: Mutex::new(String::new()),
            log: Mutex::new(VecDeque::with_capacity(LOG_CAP)),
            rx: AtomicU64::new(0),
            tx: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            admin: AtomicBool::new(false),
        })
    }

    pub fn state(&self) -> ConnState {
        *self.state.lock().unwrap()
    }

    pub fn set_state(&self, s: ConnState) {
        *self.state.lock().unwrap() = s;
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        let msg = msg.into();
        *self.error.lock().unwrap() = msg.clone();
        self.set_state(ConnState::Error);
        self.log(format!("error: {msg}"));
    }

    pub fn error(&self) -> String {
        self.error.lock().unwrap().clone()
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        log::info!("{line}");
        let mut l = self.log.lock().unwrap();
        if l.len() == LOG_CAP {
            l.pop_front();
        }
        l.push_back(line);
    }

    pub fn logs(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    pub fn reset_counters(&self) {
        self.rx.store(0, Ordering::Relaxed);
        self.tx.store(0, Ordering::Relaxed);
    }

    pub fn rx(&self) -> u64 {
        self.rx.load(Ordering::Relaxed)
    }

    pub fn tx(&self) -> u64 {
        self.tx.load(Ordering::Relaxed)
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn clear_stop(&self) {
        self.stop.store(false, Ordering::Relaxed);
    }

    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn set_admin(&self, v: bool) {
        self.admin.store(v, Ordering::Relaxed);
    }

    pub fn is_admin(&self) -> bool {
        self.admin.load(Ordering::Relaxed)
    }
}
