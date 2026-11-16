//! airdrop-helper: the privileged half of the AirDrop client. Run via pkexec.
//!
//! `up` brings AWDL up and prints JSON status lines on stdout. It tears everything down
//! when stdin reaches EOF (the client exited, crashed or was killed) or on SIGTERM.
//! Teardown is fail-safe: every restore step runs even if an earlier one fails, and
//! logging can never abort it. What was changed is recorded in a state file so
//! `recover` can undo it even if this process itself was killed.

mod args;
mod ble;
mod genl;
mod nl80211;
mod nm;
mod out;
mod owl;
mod radio;
mod session;
mod sys;

use args::{BorrowRadio, Cli, Cmd, Mode};
use clap::Parser;
use out::{emit, log};
use serde_json::json;
use session::{Session, Stop, HELPER_API};
use std::process::ExitCode;
use std::sync::atomic::Ordering;

extern "C" fn on_term(_: libc::c_int) {
    session::TERMINATE.store(true, Ordering::Relaxed);
}

/// The terminal's Ctrl-C and hang-up belong to the client: we stop when its end of our
/// stdin closes, whatever killed it, so these must not interrupt setup or teardown.
fn install_signals() {
    // SAFETY: the handler only stores to an atomic; SIG_IGN needs no handler at all.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_term as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Read-only facts for `airdrop doctor`: nothing here changes anything.
fn probe(iface: &str) {
    let phy = sys::phy_of(iface);
    let wiphy = phy.as_deref().and_then(sys::phy_index);
    let mut nl = nl80211::Nl80211::open().ok();
    let channel = nl.as_mut().and_then(|n| n.interface(iface).ok()).and_then(|i| i.freq).and_then(sys::channel_of_freq);
    let monitor = wiphy.and_then(|w| nl.as_mut()?.supports_monitor(w).ok()).unwrap_or(false);
    // Channels the kernel would silently refuse to transmit on (regulatory No-IR).
    let no_ir: Vec<u32> = radio::AWDL_CHANNELS
        .into_iter()
        .filter(|&c| wiphy.is_some_and(|w| nl.as_mut().is_some_and(|n| n.freq_is_no_ir(w, sys::freq_of_channel(c)).unwrap_or(false))))
        .collect();
    let associated = nl.as_mut().and_then(|n| n.associated(iface).ok());
    let nm = nm::Nm::connect().ok();
    let connection = nm.as_ref().and_then(|n| n.active_connection(iface).ok().flatten()).map(|(_, name)| name);
    emit("probe", json!({
        "api": HELPER_API, "iface": iface, "phy": phy, "owl": owl::find(), "channel": channel, "monitor": monitor,
        "no_ir": no_ir, "associated": associated, "nm_state": nm.as_ref().and_then(|n| n.device_state(iface).ok()),
        "connection": connection, "ht_driver": owl::ht_driver_loaded(),
    }));
}

fn up(args: &args::Up) -> ExitCode {
    out::set_verbose(args.verbose);
    log(&format!("---- up {} mode={:?} channel={}", args.iface, args.mode, args.channel));
    if args.go_channel.is_some() || args.borrow_radio == BorrowRadio::Roc || args.mode == Mode::Exclusive {
        emit("error", json!({ "message": "exclusive mode, --go-channel and --borrow-radio roc were removed in the Rust helper: \
                                          they take Wi-Fi down or were refuted on this chip. Use --mode borrow" }));
        return ExitCode::from(1);
    }
    // Held until we exit: a second `up` must not "restore" this live session and kill its OWL.
    let _lock = match sys::lock_session() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            emit("error", json!({ "message": "another AirDrop session is running (stop it first, or if it hangs: \
                                              sudo pkill -x airdrop-helper)" }));
            return ExitCode::from(1);
        }
        Err(e) => {
            emit("error", json!({ "message": format!("cannot lock {}: {e}", sys::LOCK_FILE) }));
            return ExitCode::from(1);
        }
    };
    install_signals();
    let mut session = match Session::new(args) {
        Ok(s) => s,
        Err(e) => {
            emit("error", json!({ "message": e }));
            return ExitCode::from(1);
        }
    };
    let mut code = ExitCode::SUCCESS;
    match session.up() {
        Ok(()) => {
            if !session.wait() {
                code = ExitCode::from(1);
            }
        }
        Err(Stop::Interrupted) => {}
        Err(Stop::Failed(e)) => {
            emit("error", json!({ "message": e }));
            code = ExitCode::from(1);
        }
    }
    // Nothing may interrupt this: from here a second SIGTERM is ignored.
    // SAFETY: SIG_IGN needs no handler.
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    session.down();
    emit("down", json!({}));
    code
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if matches!(cli.cmd, Cmd::Api) {
        println!("{HELPER_API}");
        return ExitCode::SUCCESS;
    }
    if !is_root() {
        emit("error", json!({ "message": "airdrop-helper must run as root (via pkexec)" }));
        return ExitCode::from(1);
    }
    match cli.cmd {
        Cmd::Api => unreachable!("handled above"),
        Cmd::Probe(a) => probe(&a.iface),
        Cmd::Recover(a) => {
            // A live session undoes its own changes; recovering under it would pull the radio
            // from a running transfer. A dead helper's lock is gone with it.
            let Ok(Some(_lock)) = sys::lock_session() else {
                emit("error", json!({ "message": "an AirDrop session is still running; stop it first \
                                                  (if it hangs: sudo pkill -x airdrop-helper, then recover)" }));
                return ExitCode::from(1);
            };
            install_signals();
            session::recover(&a.iface);
        }
        Cmd::Up(args) => return up(&args),
    }
    ExitCode::SUCCESS
}
