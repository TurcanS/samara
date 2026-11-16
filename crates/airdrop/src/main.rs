//! airdrop: command-line AirDrop for Linux.
//!
//! Exit codes: 0 done, 1 failed, 2 usage or configuration error, 3 declined by the other
//! side, 4 nobody found, 130 stopped with Ctrl-C / SIGTERM.

mod ble;
mod cli;
mod config;
mod conn;
mod dump;
mod link;
mod link_cmds;
mod mdns;
mod progress;
mod prompt;
mod receiver;
mod sender;
mod stop;
mod tls;
mod util;

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// A `-v` line on stderr.
#[macro_export]
macro_rules! verbose {
    ($($arg:tt)*) => {
        if $crate::is_verbose() {
            eprintln!("DEBUG airdrop: {}", format_args!($($arg)*));
        }
    };
}

fn main() {
    use clap::Parser;
    let args = cli::Cli::parse();
    VERBOSE.store(args.global.verbose, Ordering::Relaxed);
    let code = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt.block_on(cli::run(args)),
        Err(e) => {
            eprintln!("cannot start: {e}");
            1
        }
    };
    // Not a return from main: a thread blocked reading the terminal must not hold up exit.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
