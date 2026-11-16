//! Output that can never raise: the JSON status lines on stdout, the verbose log
//! on stderr and the log file in /run. A vanished terminal must not stop teardown.

use serde_json::{json, Map, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub const HELPER_LOG: &str = "/run/airdrop-helper.log";

static VERBOSE: AtomicBool = AtomicBool::new(false);
static STDERR_OK: AtomicBool = AtomicBool::new(true);
static STDOUT_OK: AtomicBool = AtomicBool::new(true);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

fn timestamp() -> String {
    // SAFETY: localtime_r only writes into the zeroed tm we pass.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

fn logfile(line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(HELPER_LOG) {
        let _ = writeln!(f, "{} {line}", timestamp());
    }
}

pub fn log(msg: &str) {
    logfile(msg);
    if VERBOSE.load(Ordering::Relaxed) && STDERR_OK.load(Ordering::Relaxed) {
        let mut err = std::io::stderr();
        if writeln!(err, "[helper] {msg}").and_then(|()| err.flush()).is_err() {
            STDERR_OK.store(false, Ordering::Relaxed); // terminal gone: keep going silently
        }
    }
}

/// One JSON status line, `{"state": <state>, ...fields}`.
pub fn emit(state: &str, fields: Value) {
    let mut obj = Map::new();
    obj.insert("state".into(), json!(state));
    if let Value::Object(extra) = fields {
        obj.extend(extra);
    }
    let line = Value::Object(obj).to_string();
    logfile(&format!("emit {line}"));
    if STDOUT_OK.load(Ordering::Relaxed) {
        let mut out = std::io::stdout();
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            STDOUT_OK.store(false, Ordering::Relaxed);
        }
    }
}

pub fn warn(message: &str) {
    emit("warning", json!({ "message": message }));
}

pub fn notice(message: &str) {
    emit("notice", json!({ "message": message }));
}
