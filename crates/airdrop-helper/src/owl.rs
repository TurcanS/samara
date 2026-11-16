//! OWL (the AWDL daemon, C++, untouched): find it, build its command line from the
//! session settings, spawn it and supervise it. Every tuning the Python helper had
//! is kept here, option by option, with the measurement that justified it.

use crate::out::{log, notice, warn};
use crate::radio::AWDL_IFACE;
use crate::sys::{self, OWL_PATHS};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

pub const OWL_LOG: &str = "/run/airdrop-owl.log";
/// iPhones leave our channel for 10-14 s at a time (measured): keep them as peers
/// (and their neighbor entry) across such gaps.
const PEER_TIMEOUT_MS: u32 = 10_000;
const HT_DRIVER_MARK: &str = "/run/airdrop-mt7921-ht";
/// srcversion of the HT-capable mt7921e builds known by name (see driver/README.md). Add the value
/// after rebuilding the module for another kernel, then reinstall the helper.
const HT_DRIVER_SRCVERSIONS: [&str; 1] = ["199C7E82F589E3F103CBEC5"];
/// Only the build with Block Ack for AWDL peers has this parameter (E-TXAGG); it is the HT
/// build plus that, whatever its srcversion.
pub const AGG_DRIVER_PARAM: &str = "/sys/module/mt7921e/parameters/awdl_ba";

pub fn agg_driver_loaded() -> bool {
    std::path::Path::new(AGG_DRIVER_PARAM).exists()
}

/// The first root-owned, not group/world-writable OWL: root runs it.
pub fn find() -> Option<String> {
    OWL_PATHS.iter().find(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0 && m.mode() & 0o111 != 0)
    }).map(|p| (*p).to_string())
}

fn loaded_srcversion() -> Option<String> {
    std::fs::read_to_string("/sys/module/mt7921e/srcversion").ok().map(|s| s.trim().to_string())
}

/// True when the loaded mt7921e sends injected frames at HT rates. A reboot or the
/// stock driver leaves no match, and OWL stays on the legacy rates every build can send.
pub fn ht_driver_loaded() -> bool {
    let Some(loaded) = loaded_srcversion() else { return false };
    if HT_DRIVER_SRCVERSIONS.contains(&loaded.as_str()) || agg_driver_loaded() {
        return true;
    }
    let Ok(meta) = std::fs::metadata(HT_DRIVER_MARK) else { return false };
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return false;
    }
    std::fs::read_to_string(HT_DRIVER_MARK).is_ok_and(|want| !want.trim().is_empty() && want.trim() == loaded)
}

/// The distribution's own mt7921e (same srcversion as the file modprobe would load).
pub fn stock_driver_loaded() -> bool {
    let Some(loaded) = loaded_srcversion() else { return false };
    sys::run(sys::tool("modinfo"), &["-F", "srcversion", "mt7921e"]).is_some_and(|(_, out)| out.trim() == loaded)
}

pub struct Binary {
    pub path: String,
    contents: Vec<u8>,
}

impl Binary {
    pub fn open(path: String) -> std::io::Result<Self> {
        Ok(Self { contents: std::fs::read(&path)?, path })
    }

    /// Our OWL patch adds options one at a time; an older build exits on unknown
    /// ones, so look for each option's own usage string.
    pub fn has(&self, usage: &[u8]) -> bool {
        self.contents.windows(usage.len()).any(|w| w == usage)
    }
}

/// Everything that decides OWL's command line.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub vif: String,
    pub channel: u32,
    pub borrow: bool,
    pub intersect: bool,
    pub static_seq: bool,
    pub dps_single_band: bool,
    pub dps_infra_channel: bool,
    pub narrow: bool,
    pub vht80: bool,
    pub realtime: bool,
    pub tx_rate: Option<String>,
    pub ampdu: bool,
    pub no_ampdu: bool,
    pub presence: Option<u32>,
    pub election_metric: Option<u32>,
    /// The access-point interface OWL registers peers on as stations (`-G`, E-TXAGG).
    pub sta_iface: Option<String>,
    pub ba_probe: bool,
    pub verbose: bool,
}

/// `-r` value for OWL, with the notices the Python helper gave.
fn tx_rate(owl: &Binary, s: &Settings) -> Option<String> {
    // Since 2026-11-12 the default on 5 GHz is automatic: OWL starts at the fastest rate
    // and steps down while frames go unacknowledged, back up when they are. With the HT
    // driver build the ladder starts at MCS 15 / 40 MHz / short GI (43 MB at 5.6 MB/s),
    // otherwise at 54 Mbit/s (1.0 MB/s). Before that: 24 Mbit/s = 390 KB/s, 54 = 676 KB/s.
    let mut rate = s.tx_rate.clone();
    if rate.is_none() && s.channel >= 36 {
        rate = Some(if owl.has(b"auto / auto-ht") {
            if ht_driver_loaded() { "auto-ht" } else { "auto" }
        } else {
            "54"
        }.to_string());
        if s.borrow && stock_driver_loaded() {
            notice("the stock Wi-Fi driver is loaded: sending to an iPhone will be several times slower (see `airdrop doctor`)");
        }
    }
    let rate = rate.filter(|r| r != "12")?;
    if owl.has(b"-r wants") {
        Some(rate)
    } else {
        if s.tx_rate.is_some() {
            warn("the installed OWL has no -r: rebuild and reinstall it");
        }
        None
    }
}

/// One optional flag: added when OWL knows it; a warning if the user asked for it and it does not.
fn flag(owl: &Binary, cmd: &mut Vec<String>, usage: &[u8], flag: &str, wanted: bool, asked: bool) {
    if !wanted {
        return;
    }
    if owl.has(usage) {
        cmd.push(flag.to_string());
    } else if asked {
        warn(&format!("the installed OWL has no {flag}: rebuild and reinstall it"));
    }
}

pub fn command_line(owl: &Binary, s: &Settings) -> Vec<String> {
    let mut cmd = vec![owl.path.clone(), "-i".into(), s.vif.clone(), "-c".into(), s.channel.to_string(), "-N".into()];
    if owl.has(b"-P wants 500..60000") {
        cmd.extend(["-P".into(), PEER_TIMEOUT_MS.to_string()]);
        // Radio pinned to `channel`: only advertise the sync master's slots we can serve.
        if s.intersect || s.borrow {
            cmd.push("-I".into());
        }
    } else {
        warn("the installed OWL is older than this helper (no -I/-P): rebuild and reinstall it \
              (scripts/build-owl.sh, then install to /usr/local/lib/airdrop/owl)");
    }
    if s.static_seq {
        cmd.push("-F".into());
    }
    // Borrow mode is genuinely single-band on one channel; advertising that makes the
    // iPhone keep more slots here: 6 MB -> 9.9 MB and no retry storm (E0.11 runs 3/5 vs 6).
    flag(owl, &mut cmd, b"DPS: single band", "-S", s.dps_single_band || s.borrow, s.dps_single_band);
    // HT40 HT caps: default in borrow on 5 GHz (matches the HT40 GO), unless --narrow.
    // Lets the iPhone send at HT40 (300 Mbit/s); E2.10, 43 MB video.
    let ht40 = !s.narrow && s.borrow && s.channel >= 36;
    flag(owl, &mut cmd, b"HT caps: 40 MHz", "-H", ht40, ht40);
    // VHT caps in TLV 0x11, only while the GO runs at 80 MHz (borrow default on 149-161 since E-VHT80).
    let vht80 = s.vht80 && ht40;
    flag(owl, &mut cmd, b"VHT caps: 80 MHz", "-V", vht80, vht80);
    // Real-time DPS bit: +45% airtime, 1.5% -> 2.2% duty, 17.6 s -> 14.3 s on 11.5 MB (E2.10).
    flag(owl, &mut cmd, b"real-time bit set", "-R", s.realtime || s.borrow, s.realtime);
    if (s.dps_infra_channel || s.borrow) && owl.has(b"DPS: infra channel") {
        cmd.extend(["-C".into(), s.channel.to_string()]); // the channel AWDL runs on
    } else if s.dps_infra_channel {
        warn("the installed OWL has no -C: rebuild and reinstall it");
    }
    if let Some(rate) = tx_rate(owl, s) {
        cmd.extend(["-r".into(), rate]);
    }
    // Accept the iPhone's Block Ack request so it sends A-MPDUs: 43.4 MB in 13.5 s
    // (3.2 MB/s) against 30.3 s without (P2.3). Default in borrow mode; --no-ampdu switches it off.
    if !s.no_ampdu {
        flag(owl, &mut cmd, b"Block Ack: accepting", "-A", s.ampdu || s.borrow, s.ampdu);
    }
    // Presence window: unicast to a peer goes out when a frame from it arrived within the
    // last N ms even if OWL's slot estimate says "not yet" (P2.4: TCP ACKs waited 33-43 ms
    // behind the slot gate at the start of every visit). 16 ms default in borrow mode.
    let presence = s.presence.unwrap_or(if s.borrow { 16 } else { 0 });
    if presence > 0 {
        if owl.has(b"-p wants 1..100") {
            cmd.extend(["-p".into(), presence.to_string()]);
        } else if s.presence.is_some_and(|p| p > 0) {
            warn("the installed OWL has no -p: rebuild and reinstall it");
        }
    }
    flag(owl, &mut cmd, b"Block Ack probe: asking", "-B", s.ba_probe, s.ba_probe);
    if let Some(iface) = &s.sta_iface {
        if owl.has(b"stations: peers we send to") {
            cmd.extend(["-G".into(), iface.clone()]);
        } else {
            warn("the installed OWL has no -G: rebuild and reinstall it (sending without aggregation)");
        }
    }
    if let Some(m) = s.election_metric {
        if owl.has(b"-M wants 1..1000") {
            cmd.extend(["-M".into(), m.to_string()]);
        } else {
            warn("the installed OWL has no -M: rebuild and reinstall it");
        }
    }
    if s.verbose {
        cmd.push("-v".into());
    }
    cmd
}

pub struct Owl {
    child: Child,
}

/// What OWL's log says about the peers right now: whether any is present, and the channel
/// sequence the last of them announced (16 slots; 0 = not listening).
pub struct PeerView {
    pub present: bool,
    pub sequence: Option<Vec<u32>>,
}

pub fn peer_view() -> PeerView {
    parse_peer_view(&std::fs::read_to_string(OWL_LOG).unwrap_or_default())
}

fn parse_peer_view(text: &str) -> PeerView {
    let mut peers: Vec<(String, Option<Vec<u32>>)> = Vec::new();
    let mut last: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.split("remove peer ").nth(1) {
            let mac = rest.split_whitespace().next().unwrap_or_default();
            peers.retain(|(m, _)| m != mac);
        } else if let Some(rest) = line.split("add peer ").nth(1) {
            let mac = rest.split_whitespace().next().unwrap_or_default().to_string();
            if !peers.iter().any(|(m, _)| *m == mac) {
                peers.push((mac, None));
            }
        } else if let Some((head, seq)) = line.split_once(" changed channel sequence to ") {
            let mac = head.split("peer ").nth(1).and_then(|r| r.split_whitespace().next()).unwrap_or_default().to_string();
            let seq: Vec<u32> = seq.trim().split(',').filter_map(|n| n.trim().parse().ok()).collect();
            if seq.len() != 16 {
                continue;
            }
            match peers.iter_mut().find(|(m, _)| *m == mac) {
                Some(p) => p.1 = Some(seq),
                None => peers.push((mac.clone(), Some(seq))),
            }
            last = Some(mac);
        }
    }
    let sequence = last.and_then(|mac| peers.iter().find(|(m, _)| *m == mac).and_then(|(_, s)| s.clone()))
        .or_else(|| peers.iter().rev().find_map(|(_, s)| s.clone()));
    PeerView { present: !peers.is_empty(), sequence }
}

fn log_tail() -> String {
    let Ok(text) = std::fs::read_to_string(OWL_LOG) else { return "no log".into() };
    let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with(' ')).map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(4)..].join(" | ");
    if tail.is_empty() { "no output".into() } else { tail }
}

impl Owl {
    /// Spawn OWL and wait for `awdl0`. `fast` waits 0.4 s instead of 1 s for it to settle.
    pub fn start(cmd: &[String], fast: bool) -> Result<Self, String> {
        log(&format!("start: {}", cmd.join(" ")));
        let logf = File::create(OWL_LOG).map_err(|e| format!("cannot write {OWL_LOG}: {e}"))?;
        let child = sys::command(&cmd[0])
            .args(&cmd[1..])
            .stdout(Stdio::from(logf.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(logf))
            .spawn()
            .map_err(|e| format!("cannot start OWL: {e}"))?;
        let mut owl = Self { child };
        match owl.wait_for_interface(fast) {
            Ok(()) => Ok(owl),
            Err(e) => {
                owl.stop(); // never leave a half-started OWL behind
                Err(e)
            }
        }
    }

    fn wait_for_interface(&mut self, fast: bool) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.check()?;
            if sys::iface_exists(AWDL_IFACE) {
                std::thread::sleep(Duration::from_millis(if fast { 400 } else { 1000 }));
                return self.check();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("OWL did not create {AWDL_IFACE}: {}", log_tail()))
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn check(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => Err(format!("OWL exited: {}", log_tail())),
            Ok(None) => Ok(()),
        }
    }

    /// `Some(reason)` once OWL has died.
    pub fn died(&mut self) -> Option<String> {
        self.check().err().map(|_| format!("OWL stopped: {}", log_tail()))
    }

    pub fn stop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            sys::terminate(self.child.id(), Duration::from_secs(5));
            let _ = self.child.wait();
        }
    }
}
