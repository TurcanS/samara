//! The unprivileged side of the AWDL link: runs the root helper through pkexec.
//!
//! The helper lives exactly as long as its stdin pipe (docs/HELPER.md): closing it, or
//! this process dying however it dies, makes the helper stop OWL and give Wi-Fi back.
//!
//! ```text
//! Starting ──"up"──▶ Up ──close()/stop──▶ Closing ──helper exits──▶ Closed
//!                     └──helper "error" (OWL died, Wi-Fi came back)──▶ Failed ──▶ Closing
//! ```

use crate::config::{Channel, Config, Mode};
use crate::stop::Stop;
use crate::util::which;
use serde_json::Value;
use std::net::Ipv6Addr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub const SYSTEM_HELPER: &str = "/usr/lib/airdrop/airdrop-helper";
/// Bumped whenever the helper gains something the client relies on (docs/HELPER.md §5).
pub const HELPER_API: u32 = 11;
/// HT-capable mt7921e builds (as in the helper).
pub const HT_DRIVER_SRCVERSIONS: [&str; 1] = ["199C7E82F589E3F103CBEC5"];
const AUTH_TIMEOUT: Duration = Duration::from_secs(180);
const ADDR_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub enum LinkError {
    Failed(String),
    /// Ctrl-C while the link was coming up (it was taken down again).
    Interrupted,
}

impl From<String> for LinkError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

/// `$AIRDROP_HELPER`, the installed helper, or the one built next to this binary.
pub fn find_helper() -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = crate::util::env("AIRDROP_HELPER").map(PathBuf::from).into_iter().collect();
    candidates.push(SYSTEM_HELPER.into());
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        candidates.push(dir.join("airdrop-helper"));
    }
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| "airdrop-helper not found; install it or set AIRDROP_HELPER".into())
}

/// True for a `#!` script: the retired Python helper.
pub fn is_script(path: &std::path::Path) -> bool {
    let mut head = [0u8; 2];
    std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && &head == b"#!"
}

/// API level of a helper: the Python script declares it in its source, the Rust binary
/// prints it (`airdrop-helper api`, no privileges needed). 0 = unknown.
pub fn helper_api(path: &std::path::Path) -> u32 {
    if is_script(path) {
        return std::fs::read_to_string(path).ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("HELPER_API = ").and_then(|v| v.trim().parse().ok())))
            .unwrap_or(0);
    }
    std::process::Command::new(path).arg("api").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

/// First usable (not tentative, not DAD-failed) fe80:: address of `iface`.
pub fn link_local_address(iface: &str) -> Option<Ipv6Addr> {
    std::fs::read_to_string("/proc/net/if_inet6").ok()?.lines().find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 6 || f[5] != iface || u32::from_str_radix(f[3], 16).ok()? != 0x20 {
            return None;
        }
        if u32::from_str_radix(f[4], 16).ok()? & (0x40 | 0x08) != 0 {
            return None;
        }
        u128::from_str_radix(f[0], 16).ok().map(Ipv6Addr::from)
    })
}

/// Link-local addresses of AWDL peers: OWL adds a neighbour entry on awdl0 for every peer
/// it syncs with. Read-only, no privileges.
pub fn awdl_neighbors(iface: &str) -> Vec<Ipv6Addr> {
    let Ok(out) = std::process::Command::new("ip").args(["-j", "-6", "neigh", "show", "dev", iface]).stderr(Stdio::null()).output() else {
        return vec![];
    };
    let entries: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    entries.iter().filter_map(|e| {
        let failed = e["state"].as_array().is_some_and(|s| s.iter().any(|v| v == "FAILED"));
        let addr: Ipv6Addr = e["dst"].as_str()?.parse().ok()?;
        (!failed && addr.segments()[0] == 0xfe80).then_some(addr)
    }).collect()
}

/// The station's channel, from `iw dev IFACE info` (no privileges).
pub fn station_channel(iface: &str) -> Option<u32> {
    let out = std::process::Command::new("iw").args(["dev", iface, "info"]).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = &text[text.find("channel ")? + 8..];
    rest.split_whitespace().next()?.parse().ok()
}

fn pkexec() -> Result<PathBuf, String> {
    which("pkexec").ok_or_else(|| "pkexec not found (install polkit)".into())
}

/// `airdrop-helper recover`: undo whatever a crashed session left behind.
pub async fn recover(iface: &str) -> Result<Vec<String>, String> {
    let out = Command::new(pkexec()?).arg(find_helper()?).args(["recover", "--iface", iface])
        .stdin(Stdio::null()).stdout(Stdio::piped()).output().await.map_err(|e| e.to_string())?;
    if matches!(out.status.code(), Some(126 | 127)) {
        return Err("authorization was cancelled or denied".into());
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
        match msg["state"].as_str() {
            Some("recovered") => {
                return Ok(msg["steps"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default());
            }
            Some("error") => return Err(msg["message"].as_str().unwrap_or("recover failed").to_string()),
            _ => {}
        }
    }
    Err(format!("recover failed (code {:?})", out.status.code()))
}

pub struct Options {
    /// Send the BLE wake-up advert (find/send).
    pub ble: bool,
    /// Shorter bring-up waits (the listener).
    pub fast: bool,
    /// No per-session chatter (the listener prints its own lines).
    pub quiet: bool,
    /// This session uploads to a receiver: ask the helper for aggregated sending (API 11).
    pub sending: bool,
    /// Run on this channel with Wi-Fi paused, whatever the settings say: the helper reported
    /// that the iPhone is better met there (`Link::prefers`).
    pub channel: Option<u32>,
}

pub struct Link {
    child: Child,
    stdin: Option<ChildStdin>,
    pub iface: String,
    pub address: Ipv6Addr,
    pub mode: String,
    pub channel: String,
    quiet: bool,
    /// The Wi-Fi interface the session borrowed (`recover` needs it).
    wifi_iface: String,
    /// Set when the helper reports an error after `up` (or exits): the session must end.
    failed: watch::Receiver<Option<String>>,
    /// The channel the helper last said a peer would be better met on.
    prefer: watch::Receiver<Option<u32>>,
    keepalive: Option<JoinHandle<()>>,
    /// Reads the helper's output to the end, also through its teardown: a closed pipe
    /// made the Python helper print a BrokenPipeError on its way out. True once the
    /// helper said `down` (its teardown ran).
    watcher: Option<JoinHandle<bool>>,
}

fn status(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|_| serde_json::json!({ "state": "log", "message": line.trim() }))
}

impl Link {
    /// Run the helper and wait until AWDL is up and awdl0 has its address. A stop request
    /// during bring-up takes everything down again before returning.
    pub async fn up(cfg: &Config, opts: Options, stop: &Stop) -> Result<Self, LinkError> {
        let helper = find_helper()?;
        let station = station_channel(&cfg.wifi_iface);
        // auto: borrow on channel 149 (or the one asked for), where an iPhone on 5 GHz keeps
        // its AirDrop data path (E0.11/E2.10), unless the station already sits on that
        // channel: then shared costs nothing and Wi-Fi stays up.
        let want = match (opts.channel, cfg.channel) {
            (Some(c), _) | (None, Channel::Fixed(c)) => c,
            (None, Channel::Auto) => 149,
        };
        let mode = match cfg.mode {
            _ if opts.channel.is_some() => "borrow",
            Mode::Borrow => "borrow",
            Mode::Shared => "shared",
            Mode::Auto if station == Some(want) => "shared",
            Mode::Auto => {
                if !opts.quiet {
                    eprintln!("Running on AirDrop channel {want}: Wi-Fi is paused for this session and reconnects afterwards \
                               (--mode shared keeps Wi-Fi up; --channel 6 for a 2.4 GHz iPhone).");
                }
                "borrow"
            }
        };
        let mut cmd = Command::new(pkexec()?);
        let channel = opts.channel.map_or_else(|| cfg.channel.to_string(), |c| c.to_string());
        cmd.arg(&helper).args(["up", "--iface", &cfg.wifi_iface, "--mode", mode, "--channel", &channel]);
        if opts.ble {
            cmd.arg("--ble");
        }
        if crate::is_verbose() {
            cmd.arg("-v");
        }
        if opts.fast && helper_api(&helper) >= 5 {
            cmd.arg("--fast");
        }
        // Aggregated sending (E-TXAGG): 5.4 -> 30.6 MB/s on 417 MB. The helper uses it only with
        // the driver build that can do it, and sends as before otherwise.
        if opts.sending && mode == "borrow" && helper_api(&helper) >= 11 {
            cmd.arg("--tx-agg");
        }
        // Experiments only (scripts/airdrop-matrix.sh); the helper validates them.
        if let Some(extra) = crate::util::env("AIRDROP_HELPER_ARGS") {
            cmd.args(extra.split_whitespace());
        }
        if !opts.quiet {
            eprintln!("Starting AWDL (authentication may be required)...");
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| format!("cannot run pkexec: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped");
        let (fail_tx, failed) = watch::channel(None);
        let (prefer_tx, prefer) = watch::channel(None);
        let mut link = Self {
            child, stdin, iface: String::new(), address: Ipv6Addr::UNSPECIFIED, mode: String::new(), channel: "?".into(),
            quiet: opts.quiet, wifi_iface: cfg.wifi_iface.clone(), failed, prefer, keepalive: None, watcher: None,
        };
        let started = tokio::select! {
            r = link.wait_up(stdout, fail_tx, prefer_tx) => r,
            () = stop.wait() => Err(LinkError::Interrupted),
        };
        if let Err(e) = started {
            link.close().await;
            return Err(e);
        }
        let iface = link.iface.clone();
        let address = tokio::select! {
            a = async {
                let deadline = tokio::time::Instant::now() + ADDR_TIMEOUT;
                while tokio::time::Instant::now() < deadline {
                    if let Some(a) = link_local_address(&iface) {
                        return Ok(a);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(LinkError::Failed(format!("{iface} has no IPv6 link-local address")))
            } => a,
            () = stop.wait() => Err(LinkError::Interrupted),
        };
        match address {
            Ok(a) => link.address = a,
            Err(e) => {
                link.close().await;
                return Err(e);
            }
        }
        link.keepalive = Some(tokio::spawn(keepalive(link.iface.clone(), link.address)));
        Ok(link)
    }

    async fn wait_up(&mut self, stdout: ChildStdout, fail_tx: watch::Sender<Option<String>>, prefer_tx: watch::Sender<Option<u32>>)
        -> Result<(), LinkError> {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            let line = match tokio::time::timeout(AUTH_TIMEOUT, lines.next_line()).await {
                Err(_) => return Err("timed out waiting for the AWDL helper".to_string().into()),
                Ok(Ok(Some(l))) => l,
                Ok(_) => {
                    let code = self.child.wait().await.ok().and_then(|s| s.code());
                    return Err(match code {
                        Some(126 | 127) => "authorization was cancelled or denied".to_string(),
                        c => format!("AWDL helper exited (code {})", c.map_or("?".into(), |c| c.to_string())),
                    }.into());
                }
            };
            let msg = status(&line);
            let text = msg["message"].as_str().unwrap_or_default().to_string();
            match msg["state"].as_str().unwrap_or_default() {
                "up" => {
                    self.iface = msg["iface"].as_str().unwrap_or("awdl0").to_string();
                    self.mode = msg["mode"].as_str().unwrap_or("?").to_string();
                    self.channel = msg["channel"].to_string();
                    if msg["api"].as_u64().unwrap_or(0) < u64::from(HELPER_API) {
                        eprintln!("The installed airdrop-helper is older than this client, so newer speed and stability \
                                   settings are not active. Reinstall it (see `airdrop doctor`).");
                    }
                    if !self.quiet {
                        let ble = if msg["ble"].as_bool() == Some(true) { " + BLE" } else { "" };
                        eprintln!("AWDL up on {} ({} mode, channel {}{ble}).", self.iface, self.mode, self.channel);
                        if self.mode == "shared" {
                            eprintln!("Your Wi-Fi stays connected.");
                        } else {
                            eprintln!("Wi-Fi is paused for this session and reconnects when AirDrop stops (if it does not: airdrop recover).");
                        }
                    }
                    self.watcher = Some(tokio::spawn(watch_helper(lines, fail_tx, prefer_tx)));
                    return Ok(());
                }
                "error" => return Err(LinkError::Failed(if text.is_empty() { "AWDL helper failed".into() } else { text })),
                "notice" if self.quiet => {}
                "notice" | "warning" | "fallback" | "log" => eprintln!("{text}"),
                _ => {} // inject_check, down
            }
        }
    }

    /// Resolves with the reason once the helper reports a failure after `up`.
    pub async fn failed(&self) -> String {
        let mut rx = self.failed.clone();
        let reason = rx.wait_for(Option::is_some).await.map(|r| r.clone().unwrap_or_default());
        match reason {
            Ok(reason) => reason,
            Err(_) => std::future::pending().await, // the watcher is gone: we are closing
        }
    }

    /// Resolves with a channel when the helper reports that a peer would be better met
    /// there than on ours (an iPhone joined to a 5 GHz network mostly stays on that
    /// network's channel), or that nobody is left on a channel we moved to.
    pub async fn prefers(&self) -> u32 {
        let mut rx = self.prefer.clone();
        loop {
            if rx.changed().await.is_err() {
                return std::future::pending().await; // the watcher is gone: we are closing
            }
            if let Some(c) = *rx.borrow_and_update() {
                return c;
            }
        }
    }

    /// Close the helper's stdin and wait for its teardown (Wi-Fi reconnect included).
    pub async fn close(mut self) {
        if let Some(t) = self.keepalive.take() {
            t.abort();
        }
        drop(self.stdin.take());
        let waited = tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await;
        let mut down = false;
        if let Some(w) = self.watcher.take() {
            match tokio::time::timeout(Duration::from_secs(2), w).await {
                Ok(Ok(said)) => down = said,
                _ => crate::verbose!("the helper's output did not end"),
            }
        }
        match waited {
            Err(_) => eprintln!("AWDL helper did not stop in time."),
            Ok(_) if self.mode.is_empty() => {} // it never came up: nothing to undo
            // Killed or crashed before its teardown: Wi-Fi is still paused, OWL may still run.
            Ok(_) if !down => self.recover_after_crash().await,
            Ok(_) if self.quiet => {}
            Ok(_) if self.mode == "borrow" => eprintln!("AWDL stopped, Wi-Fi restored."),
            Ok(_) => eprintln!("AWDL stopped."),
        }
    }

    async fn recover_after_crash(&self) {
        eprintln!("The AWDL helper stopped without cleaning up; undoing its session (authentication may be required)...");
        match recover(&self.wifi_iface).await {
            Ok(steps) if steps.is_empty() => eprintln!("Nothing was left to undo."),
            Ok(_) if self.mode == "borrow" => eprintln!("AWDL stopped, Wi-Fi restored."),
            Ok(_) => eprintln!("AWDL stopped."),
            Err(e) => eprintln!("Could not undo it ({e}). Run `airdrop recover` to get Wi-Fi back."),
        }
    }
}

/// Helper messages after startup. An `error` (OWL died, Wi-Fi reconnected behind our
/// back) or its exit ends the session; the old client kept running on a dead link.
async fn watch_helper(mut lines: tokio::io::Lines<BufReader<ChildStdout>>, fail: watch::Sender<Option<String>>,
                      prefer: watch::Sender<Option<u32>>) -> bool {
    let mut down = false;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let msg = status(&line);
                let text = msg["message"].as_str().unwrap_or_default().to_string();
                match msg["state"].as_str() {
                    Some("error") => {
                        eprintln!("AWDL: {text}");
                        let _ = fail.send(Some(text));
                    }
                    Some("warning") => eprintln!("AWDL: {text}"),
                    Some("prefer") => {
                        let _ = prefer.send(msg["channel"].as_u64().and_then(|c| u32::try_from(c).ok()));
                    }
                    Some("down") => down = true,
                    _ => {}
                }
            }
            _ => {
                let _ = fail.send(Some("the AWDL helper stopped".into()));
                return down;
            }
        }
    }
}

/// owly OWLY-2: an ICMPv6 echo to every AWDL peer every 3 s, so Apple peers keep a fresh
/// neighbour entry for us (their data path to us depends on it). Failures are expected.
async fn keepalive(iface: String, own: Ipv6Addr) {
    let Some(ping) = which("ping") else { return };
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let peers: Vec<Ipv6Addr> = {
            let iface = iface.clone();
            tokio::task::spawn_blocking(move || awdl_neighbors(&iface)).await.unwrap_or_default()
        };
        let pings = peers.into_iter().filter(|a| *a != own).take(8).filter_map(|a| {
            Command::new(&ping).args(["-6", "-n", "-c", "1", "-W", "1", &format!("{a}%{iface}")])
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn().ok()
        }).collect::<Vec<_>>();
        for mut p in pings {
            let _ = tokio::time::timeout(Duration::from_secs(3), p.wait()).await;
        }
    }
}
