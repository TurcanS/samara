//! Single-line transfer progress on stderr; `add` may be called from any thread.

use crate::util::{human, stderr_is_tty};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub struct Progress {
    total: Option<u64>,
    label: String,
    done: AtomicU64,
    enabled: AtomicBool,
    start: Instant,
    last_draw: Mutex<Instant>,
}

impl Progress {
    pub fn new(total: Option<u64>, label: String) -> Self {
        let start = Instant::now();
        Self {
            total: total.filter(|&t| t > 0),
            label,
            done: AtomicU64::new(0),
            enabled: AtomicBool::new(stderr_is_tty()),
            start,
            last_draw: Mutex::new(start),
        }
    }

    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Hide the line (while a question is on screen) or show it again.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
        if !self.enabled() {
            return;
        }
        let Ok(mut last) = self.last_draw.try_lock() else { return };
        let now = Instant::now();
        if now.duration_since(*last).as_secs_f64() >= 0.2 {
            *last = now;
            self.render(now);
        }
    }

    fn render(&self, now: Instant) {
        let elapsed = now.duration_since(self.start).as_secs_f64().max(1e-6);
        let done = self.done() as f64;
        let rate = done / elapsed;
        let line = match self.total {
            Some(total) => {
                let total = total as f64;
                let pct = (100.0 * done / total).min(100.0);
                let eta = if rate > 0.0 { (total - done) / rate } else { 0.0 };
                format!("\r{}: {pct:5.1}%  {} / {}  {}/s  ETA {eta:4.0}s ", self.label, human(done), human(total), human(rate))
            }
            None => format!("\r{}: {}  {}/s ", self.label, human(done), human(rate)),
        };
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{line}\x1b[K");
        let _ = err.flush();
    }

    /// Draw a last time and end the line; returns the seconds elapsed.
    pub fn finish(&self) -> f64 {
        let now = Instant::now();
        if self.enabled() {
            self.render(now);
            eprintln!();
        }
        now.duration_since(self.start).as_secs_f64().max(1e-6)
    }
}
