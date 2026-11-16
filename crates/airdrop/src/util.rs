//! Small shared pieces: randomness, human-readable sizes, safe printing, env knobs.

use std::time::{Duration, SystemTime};

/// Bytes from the kernel's random source. Panics only if the kernel refuses, which
/// would make every identity we generate predictable: better to stop.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    let mut filled = 0;
    while filled < N {
        // SAFETY: getrandom writes at most N - filled bytes into the rest of buf.
        let n = unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), N - filled, 0) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            assert!(e.kind() == std::io::ErrorKind::Interrupted, "getrandom failed: {e}");
        } else {
            filled += n as usize;
        }
    }
    buf
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A random (version 4) UUID, upper case, as iOS writes TransferIDs.
pub fn uuid4_upper() -> String {
    let mut b: [u8; 16] = random_bytes();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b).to_uppercase();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Decimal units, as the old client printed them (scripts/analyze-trace.py parses this).
pub fn human(n: f64) -> String {
    let mut n = n;
    for unit in ["B", "KB", "MB"] {
        if n < 1000.0 {
            return if unit == "B" { format!("{n:.0} B") } else { format!("{n:.1} {unit}") };
        }
        n /= 1000.0;
    }
    format!("{n:.1} GB")
}

/// Text from the network made safe for a terminal and for line-based parsers: control
/// characters (escape sequences, newlines that could fake a `SAVED ` line) are dropped.
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// `HH:MM:SS` local time.
pub fn clock() -> String {
    // SAFETY: localtime_r only writes into the zeroed tm we pass.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// `YYYYmmdd-HHMMSS` local time, for session folders.
pub fn stamp() -> String {
    // SAFETY: as in clock().
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!("{:04}{:02}{:02}-{:02}{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

/// ISO 8601 local time with milliseconds, for dump records.
pub fn iso_now() -> String {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    // SAFETY: as in clock().
    unsafe {
        let secs = now.as_secs() as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday,
            tm.tm_hour, tm.tm_min, tm.tm_sec, now.subsec_millis())
    }
}

pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// A seconds value from the environment; a bad value warns once and uses the default.
pub fn env_secs(name: &str, default: f64) -> Duration {
    let secs = match env(name) {
        None => default,
        Some(v) => match v.parse::<f64>() {
            Ok(s) if s.is_finite() && s >= 0.0 => s,
            _ => {
                eprintln!("{name}={v:?} is not a number of seconds; using {default}");
                default
            }
        },
    };
    Duration::from_secs_f64(secs)
}

/// An integer written like Python's `int(x, 0)`: decimal, 0x, 0o or 0b.
pub fn parse_int(text: &str) -> Option<u64> {
    let t = text.trim().replace('_', "");
    let lower = t.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(o) = lower.strip_prefix("0o") {
        u64::from_str_radix(o, 8).ok()
    } else if let Some(b) = lower.strip_prefix("0b") {
        u64::from_str_radix(b, 2).ok()
    } else {
        t.parse().ok()
    }
}

pub fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(Into::into).unwrap_or_else(|| "/".into())
}

pub fn expand_user(path: &str) -> std::path::PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => path.into(),
    }
}

pub fn stderr_is_tty() -> bool {
    // SAFETY: isatty has no preconditions.
    unsafe { libc::isatty(2) == 1 }
}

/// The program on PATH, if any.
pub fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
