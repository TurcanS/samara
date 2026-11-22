//! Small kernel-facing helpers: sysfs, link ioctls, mounts, processes.

use crate::out::log;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

pub const DEBUGFS: &str = "/sys/kernel/debug";
pub const OWL_PATHS: [&str; 4] =
    ["/usr/lib/airdrop/owl", "/usr/local/lib/airdrop/owl", "/usr/local/bin/owl", "/usr/bin/owl"];

pub fn iface_exists(name: &str) -> bool {
    fs::metadata(format!("/sys/class/net/{name}")).is_ok()
}

/// `phy1` for `wlan0`, from sysfs (the index changes between boots).
pub fn phy_of(iface: &str) -> Option<String> {
    let link = fs::read_link(format!("/sys/class/net/{iface}/phy80211")).ok()?;
    Some(link.file_name()?.to_string_lossy().into_owned())
}

/// The kernel driver bound to an interface's device (`mt7921e`, `rtw89_8852be`, `iwlwifi`, ...).
/// Monitor and GO interfaces share the device of the station interface.
pub fn driver_of(iface: &str) -> Option<String> {
    let link = fs::read_link(format!("/sys/class/net/{iface}/device/driver")).ok()?;
    Some(link.file_name()?.to_string_lossy().into_owned())
}

pub fn phy_index(phy: &str) -> Option<u32> {
    phy.strip_prefix("phy")?.parse().ok()
}

pub fn mac_of(iface: &str) -> Option<String> {
    Some(fs::read_to_string(format!("/sys/class/net/{iface}/address")).ok()?.trim().to_string())
}

pub fn parse_mac(mac: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = mac.split(':');
    for b in &mut out {
        *b = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

pub fn read_int(path: &str) -> Option<i64> {
    fs::read_to_string(path).ok()?.split_whitespace().next()?.parse().ok()
}

pub fn write_str(path: &str, value: &str) -> io::Result<()> {
    log(&format!("write {path} = {value}"));
    fs::write(path, value)
}

pub fn kernel_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn channel_of_freq(freq: u32) -> Option<u32> {
    match freq {
        2412..=2472 => Some((freq - 2407) / 5),
        2484 => Some(14),
        5000..=5895 => Some((freq - 5000) / 5),
        _ => None,
    }
}

pub fn freq_of_channel(channel: u32) -> u32 {
    if channel <= 14 {
        2407 + 5 * channel
    } else {
        5000 + 5 * channel
    }
}

// ---- link ioctls (replace `ip link set`) ----

#[repr(C)]
struct IfReq {
    name: [u8; libc::IFNAMSIZ],
    data: IfReqData,
}

#[repr(C)]
union IfReqData {
    flags: libc::c_short,
    hwaddr: libc::sockaddr,
    pad: [u8; 24],
}

fn ctl_socket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2); the fd is owned right away.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh, valid descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn ifreq(name: &str) -> io::Result<IfReq> {
    if name.len() >= libc::IFNAMSIZ {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "interface name too long"));
    }
    // SAFETY: IfReq is plain old data; all-zero is valid.
    let mut req: IfReq = unsafe { std::mem::zeroed() };
    req.name[..name.len()].copy_from_slice(name.as_bytes());
    Ok(req)
}

fn ioctl(fd: &OwnedFd, request: libc::c_ulong, req: &mut IfReq) -> io::Result<()> {
    // SAFETY: req is a valid, initialised ifreq for the duration of the call.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request as _, std::ptr::from_mut(req)) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn set_link_up(name: &str) -> io::Result<()> {
    let fd = ctl_socket()?;
    let mut req = ifreq(name)?;
    ioctl(&fd, libc::SIOCGIFFLAGS, &mut req)?;
    // SAFETY: SIOCGIFFLAGS filled the `flags` member.
    unsafe { req.data.flags |= libc::IFF_UP as libc::c_short };
    ioctl(&fd, libc::SIOCSIFFLAGS, &mut req)
}

pub fn set_mac(name: &str, mac: &[u8; 6]) -> io::Result<()> {
    let fd = ctl_socket()?;
    let mut req = ifreq(name)?;
    // The kernel wants the device's own hardware type back as the address family
    // (a monitor vif is ARPHRD_IEEE80211_RADIOTAP, not ARPHRD_ETHER): read it first.
    ioctl(&fd, libc::SIOCGIFHWADDR, &mut req)?;
    // SAFETY: SIOCGIFHWADDR filled the `hwaddr` member; we only replace the address bytes.
    unsafe {
        for (dst, src) in req.data.hwaddr.sa_data.iter_mut().zip(mac) {
            *dst = *src as libc::c_char;
        }
    }
    ioctl(&fd, libc::SIOCSIFHWADDR, &mut req)
}

// ---- mounts ----

pub fn is_mount(path: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let parent = std::path::Path::new(path).parent().map_or("/".into(), |p| p.to_path_buf());
    match (fs::metadata(path), fs::metadata(parent)) {
        (Ok(a), Ok(b)) => a.dev() != b.dev(),
        _ => false,
    }
}

pub fn mount_debugfs() -> io::Result<()> {
    let src = c"none";
    let target = std::ffi::CString::new(DEBUGFS).expect("no NUL");
    // SAFETY: all pointers are valid NUL-terminated strings; data is null.
    let rc = unsafe { libc::mount(src.as_ptr(), target.as_ptr(), c"debugfs".as_ptr(), 0, std::ptr::null()) };
    if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

pub fn umount(path: &str) -> io::Result<()> {
    let target = std::ffi::CString::new(path).expect("no NUL");
    // SAFETY: valid NUL-terminated string.
    if unsafe { libc::umount(target.as_ptr()) } < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

// ---- processes ----

pub const ENV_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

pub fn tool(name: &str) -> Option<String> {
    ["/usr/bin", "/usr/sbin", "/bin", "/sbin"]
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| fs::metadata(p).map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0).unwrap_or(false))
}

/// A command with our fixed environment, no stdin and its own session: the terminal's
/// Ctrl-C goes to the foreground process group, and a child that handles it itself
/// dies of it (seen 2026-11-11: a reconnect "terminated by signal Interrupt").
pub fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear().env("PATH", ENV_PATH).env("LANG", "C").stdin(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no shared state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd
}

/// Run a short tool to completion (10 s at most, then it is killed); returns
/// (success, stdout) and logs failures.
pub fn run(program: Option<String>, args: &[&str]) -> Option<(bool, String)> {
    use std::io::Read;
    let program = program?;
    log(&format!("run: {program} {}", args.join(" ")));
    let mut child = command(&program).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                log("  -> timed out, killed");
                let _ = child.kill();
                let _ = child.wait();
                return Some((false, String::new()));
            }
        }
    };
    // Our tools print a few lines at most, far below the pipe buffer.
    let mut out = String::new();
    let _ = child.stdout.take()?.read_to_string(&mut out);
    if !status.success() {
        log(&format!("  -> {status}"));
    }
    Some((status.success(), out))
}

pub fn pid_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        // field 3 is the state; a zombie is as good as dead
        Ok(stat) => stat.rsplit(')').next().and_then(|r| r.split_whitespace().next()) != Some("Z"),
        Err(_) => false,
    }
}

pub fn signal(pid: u32, sig: libc::c_int) {
    // 0 and -1 would signal a process group or everything; 1 is init. A recorded pid that
    // does not fit a pid_t is garbage.
    let Ok(pid) = libc::pid_t::try_from(pid) else { return };
    if pid <= 1 {
        return;
    }
    // SAFETY: kill(2) with a positive pid we recorded; at worst the process is already gone.
    unsafe { libc::kill(pid, sig) };
}

/// The short name of `pid` (`/proc/<pid>/comm`), to check what a recorded pid runs now.
pub fn comm_of(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim_end().to_string())
}

/// Fill `buf` from the kernel's random source.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let rest = &mut buf[filled..];
        // SAFETY: getrandom writes at most rest.len() bytes into rest.
        let n = unsafe { libc::getrandom(rest.as_mut_ptr().cast(), rest.len(), 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        } else {
            filled += n as usize;
        }
    }
    Ok(())
}

pub const LOCK_FILE: &str = "/run/airdrop-helper.lock";

/// One session at a time: an exclusive `flock` held for the life of the returned file
/// (the kernel drops it when this process dies, however it dies). `Ok(None)` means
/// another helper holds it. The fd is close-on-exec, so OWL cannot keep it alive.
/// Root-only (0600): flock needs only a readable fd, so a world-readable lock file would
/// let any user hold it and block every session.
pub fn lock_session() -> io::Result<Option<fs::File>> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(LOCK_FILE)?;
    // A file left by someone else, or with other modes, is taken back.
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    // SAFETY: fchown on an fd we own; root:root.
    if unsafe { libc::fchown(file.as_raw_fd(), 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: flock on an fd we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EWOULDBLOCK) { Ok(None) } else { Err(e) }
}

/// SIGTERM, wait up to `grace`, then SIGKILL.
pub fn terminate(pid: u32, grace: std::time::Duration) {
    signal(pid, libc::SIGTERM);
    let deadline = std::time::Instant::now() + grace;
    while pid_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if pid_alive(pid) {
        signal(pid, libc::SIGKILL);
    }
}

/// Is `pid` running one of the OWL install paths? (Pids get reused: never signal on the number alone.)
pub fn is_owl(pid: u32) -> bool {
    fs::read_link(format!("/proc/{pid}/exe"))
        .is_ok_and(|p| OWL_PATHS.iter().any(|o| p.to_string_lossy().trim_end_matches(" (deleted)") == *o))
}

/// Every running OWL, whether or not we still have it on record.
pub fn find_owl_pids() -> Vec<u32> {
    let Ok(dir) = fs::read_dir("/proc") else { return vec![] };
    dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).filter(|&pid| is_owl(pid)).collect()
}
