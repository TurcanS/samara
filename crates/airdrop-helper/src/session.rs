//! The session as an explicit state machine. Every transition is written to the
//! state file before the work it announces, so `recover` can undo it whatever
//! killed us.
//!
//! ```text
//! Idle ──borrow──▶ Borrowed ──OWL up──▶ Running ──stop──▶ TearingDown ──▶ Idle
//!   └──────────shared mode─────────────────▶ Running
//! ```
//!
//! * `Idle`: Wi-Fi is untouched and AWDL is not running (shared mode builds its
//!   monitor vifs here; they never affect the station).
//! * `Borrowed`: NetworkManager was asked to pause Wi-Fi and the radio is being
//!   taken over by our GO and vifs. Teardown must give Wi-Fi back.
//! * `Running`: AWDL (OWL) is up.
//! * `TearingDown`: restore is running; a crash here is finished by `recover`.
//!
//! The phase says where we are; the other fields of the state file say exactly
//! what to undo, and each is written before the change it describes.

use crate::args::{BleVersion, Mode, RadioLayout, Up};
use crate::ble::{self, Advertiser};
use crate::nl80211::{Nl80211, ScanEvents};
use crate::nm::{self, Nm, Paused};
use crate::out::{emit, log, notice, warn};
use crate::owl::{self, Owl};
use crate::radio::{self, ACT_IFACE, AWDL_CHANNELS, AWDL_IFACE, GO_CHANNELS, GO_IFACE, MON_IFACE};
use crate::sys::{self, DEBUGFS};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const HELPER_API: u32 = 11;
pub const STATE_FILE: &str = "/run/airdrop-helper.state";

/// Set by the SIGTERM handler; checked between steps and while waiting.
pub static TERMINATE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    Borrowed,
    Running,
    TearingDown,
}

/// What we changed. Persisted after every change; the key names match the Python
/// helper's state file, so either helper can recover the other's crash.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub iface: String,
    pub phase: Phase,
    pub vifs: Vec<String>,
    pub pm: BTreeMap<String, i64>,
    pub nm_released: bool,
    pub debugfs_mounted: bool,
    pub ble: bool,
    pub ble_pid: Option<u32>,
    pub owl_pid: Option<u32>,
    pub wlan_down: bool,
    pub nm_disconnected: Option<Paused>,
    pub nm_go_conf: bool,
    pub nm_borrow_conf: bool,
    pub go: bool,
    pub go_mac: Option<String>,
}

impl State {
    pub fn new(iface: &str) -> Self {
        Self { iface: iface.to_string(), ..Self::default() }
    }

    pub fn save(&self) {
        let tmp = format!("{STATE_FILE}.tmp");
        let result = serde_json::to_vec(self).map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|()| std::fs::rename(&tmp, STATE_FILE));
        if let Err(e) = result {
            log(&format!("cannot save state: {e}"));
        }
    }

    pub fn load() -> Option<Self> {
        serde_json::from_slice(&std::fs::read(STATE_FILE).ok()?).ok()
    }

    pub fn clear() {
        let _ = std::fs::remove_file(STATE_FILE);
    }
}

// ---- restore: every step independent ---------------------------------------

/// The steps that worked, and how long each took (for the log).
struct Steps(Vec<String>, Vec<String>);

impl Steps {
    fn run(&mut self, name: &str, f: impl FnOnce() -> std::result::Result<(), String>) {
        let started = Instant::now();
        let result = f();
        self.1.push(format!("{name} {} ms", started.elapsed().as_millis()));
        match result {
            Ok(()) => self.0.push(name.to_string()),
            Err(e) => log(&format!("restore step '{name}' failed: {e}")),
        }
    }
}

/// Where the time of a bring-up goes: one log line per session, step by step.
struct Timing {
    start: Instant,
    last: Instant,
    steps: Vec<String>,
}

impl Timing {
    fn new() -> Self {
        let now = Instant::now();
        Self { start: now, last: now, steps: vec![] }
    }

    fn mark(&mut self, what: &str) {
        let now = Instant::now();
        self.steps.push(format!("{what} +{} ms", (now - self.last).as_millis()));
        self.last = now;
    }

    fn report(&self, what: &str) {
        log(&format!("timing: {what} in {:.2} s ({})", self.start.elapsed().as_secs_f64(), self.steps.join(", ")));
    }
}

/// Undo everything `d` records, and the things worth checking even without a record
/// (a crash between "interface add" and "save"). No step can skip another.
pub fn restore(d: &State) -> Vec<String> {
    let iface = if d.iface.is_empty() { "wlan0" } else { d.iface.as_str() };
    let started = Instant::now();
    let mut steps = Steps(vec![], vec![]);
    let nm = Nm::connect().map_err(|e| log(&format!("restore: no NetworkManager bus: {e}"))).ok();
    let mut nl = Nl80211::open().map_err(|e| log(&format!("restore: no nl80211: {e}"))).ok();

    steps.run("stop owl", || {
        // The recorded pid only if it still is OWL (pids get reused), then any OWL
        // we started earlier and lost track of.
        if let Some(pid) = d.owl_pid.filter(|&p| sys::is_owl(p)) {
            sys::terminate(pid, Duration::from_secs(5));
        }
        for pid in sys::find_owl_pids() {
            sys::signal(pid, libc::SIGKILL);
        }
        Ok(())
    });
    // The BLE advert vanishes with our D-Bus connection; a leftover bluetoothctl from
    // the Python helper is the only thing to stop, and only if the pid still is one.
    if let Some(pid) = d.ble_pid.filter(|&p| sys::comm_of(p).as_deref() == Some("bluetoothctl")) {
        steps.run("ble advert (bluetoothctl)", || {
            sys::signal(pid, libc::SIGTERM);
            Ok(())
        });
    }
    for vif in [ACT_IFACE, MON_IFACE] {
        if sys::iface_exists(vif) {
            steps.run(&format!("delete {vif}"), || {
                nl.as_mut().ok_or("no nl80211")?.del_interface(vif).map_err(|e| e.to_string())
            });
        }
    }
    if sys::iface_exists(GO_IFACE) {
        steps.run("stop access point", || {
            nl.as_mut().ok_or("no nl80211")?.stop_ap(GO_IFACE).map_err(|e| e.to_string())
        });
        steps.run(&format!("delete {GO_IFACE}"), || {
            nl.as_mut().ok_or("no nl80211")?.del_interface(GO_IFACE).map_err(|e| e.to_string())
        });
    }
    for (what, path, recorded) in [
        ("networkmanager rule for the GO", nm::GO_CONF, d.nm_go_conf),
        ("networkmanager rule for borrow mode", nm::BORROW_CONF, d.nm_borrow_conf),
    ] {
        if recorded || std::path::Path::new(path).exists() {
            steps.run(what, || nm::remove_conf(nm.as_ref(), path).map_err(|e| e.to_string()));
        }
    }
    for (path, value) in &d.pm {
        steps.run(&format!("restore {}", path.rsplit('/').next().unwrap_or(path)), || {
            sys::write_str(path, &value.to_string()).map_err(|e| e.to_string())
        });
    }
    if d.debugfs_mounted {
        steps.run("unmount debugfs", || sys::umount(DEBUGFS).map_err(|e| e.to_string()));
    }
    if d.wlan_down {
        // From the retired exclusive mode; kept so an old crash can still be undone.
        steps.run("wlan type managed", || {
            nl.as_mut().ok_or("no nl80211")?.set_station_type(iface).map_err(|e| e.to_string())
        });
        steps.run("wlan up", || sys::set_link_up(iface).map_err(|e| e.to_string()));
    }
    if d.nm_released {
        steps.run("networkmanager", || {
            nm.as_ref().ok_or("no NetworkManager")?.set_managed(iface, true).map_err(|e| e.to_string())
        });
    }
    if let Some(paused) = &d.nm_disconnected {
        // Borrow mode: Wi-Fi was only disconnected (still managed, still up).
        steps.run("reconnect wi-fi", || {
            nm.as_ref().ok_or("no NetworkManager")?.reconnect(iface, paused);
            Ok(())
        });
    }
    log(&format!("restored: {}", steps.0.join(", ")));
    log(&format!("timing: restore in {:.2} s ({})", started.elapsed().as_secs_f64(), steps.1.join(", ")));
    steps.0
}

/// `airdrop-helper recover`: undo whatever a crashed session left behind.
pub fn recover(iface: &str) {
    let mut d = State::load().unwrap_or_else(|| State::new(iface));
    if d.iface.is_empty() {
        d.iface = iface.to_string();
    }
    // Even without a state file: our vifs, our OWL, power saving back on.
    if let Some(phy) = sys::phy_of(iface)
        && let Some(dir) = radio::mt76_dir(&phy) {
        for knob in radio::PM_KNOBS {
            d.pm.entry(format!("{dir}/{knob}")).or_insert(1);
        }
    }
    if owl::agg_driver_loaded() {
        d.pm.entry(owl::AGG_DRIVER_PARAM.to_string()).or_insert(0); // E-TXAGG: off outside a session
        let bw = format!("{}/awdl_bw", owl::AGG_DRIVER_PARAM.rsplit_once('/').map_or("", |p| p.0));
        if std::path::Path::new(&bw).exists() {
            d.pm.entry(bw).or_insert(0);
        }
    }
    let state = Nm::connect().and_then(|nm| nm.device_state(iface)).ok();
    if state == Some(nm::STATE_UNMANAGED) {
        d.nm_released = true;
    }
    let managed_type = Nl80211::open().and_then(|mut nl| nl.interface(iface)).map(|i| i.iftype == crate::nl80211::IFTYPE_STATION);
    if managed_type.is_ok_and(|m| !m) || !link_is_up(iface) {
        d.wlan_down = true;
    }
    if d.nm_disconnected.is_none() && state == Some(nm::STATE_DISCONNECTED) {
        // disconnected: let NetworkManager pick its network
        d.nm_disconnected = Some(Paused { uuid: None, name: None, autoconnect_only: false, ap: None });
    }
    let steps = restore(&d);
    State::clear();
    emit("recovered", json!({ "steps": steps }));
}

fn link_is_up(iface: &str) -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/flags"))
        .ok()
        .and_then(|s| i64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .is_some_and(|f| f & 1 != 0)
}

// ---- the session ----------------------------------------------------------------

/// Why bring-up stopped.
pub enum Stop {
    /// SIGTERM arrived: not an error, just tear down.
    Interrupted,
    Failed(String),
}

impl From<String> for Stop {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

impl From<&str> for Stop {
    fn from(s: &str) -> Self {
        Self::Failed(s.to_string())
    }
}

type Result<T> = std::result::Result<T, Stop>;

fn io<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| Stop::Failed(e.to_string()))
}

struct ScanGuard {
    stop: Arc<AtomicBool>,
    stopped: Arc<AtomicU32>,
    thread: JoinHandle<()>,
}

pub struct Session<'a> {
    args: &'a Up,
    state: State,
    nl: Nl80211,
    nm: Option<Nm>,
    owl: Option<Owl>,
    ble: Option<Advertiser>,
    guard: Option<ScanGuard>,
    borrow: bool,
    /// Set once `up()` starts changing things; until then `down()` has nothing to undo.
    touched: bool,
    borrow_mac: Option<String>,
    mac_warned: bool,
    timing: Timing,
    /// The GO runs at 80 MHz: OWL advertises VHT.
    vht80: bool,
    /// The channel AWDL runs on, once up.
    channel: u32,
    /// The channel a peer would rather meet on, and for how many checks in a row.
    prefer_seen: Option<(u32, u32)>,
    prefer_sent: Option<u32>,
    no_peer_since: Option<Instant>,
}

/// Teardown also runs if `up()` or `wait()` panics: Wi-Fi must come back regardless.
impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.down();
    }
}

impl<'a> Session<'a> {
    pub fn new(args: &'a Up) -> std::result::Result<Self, String> {
        Ok(Self {
            args,
            state: State::new(&args.iface),
            nl: Nl80211::open().map_err(|e| format!("nl80211: {e}"))?,
            nm: Nm::connect().map_err(|e| log(&format!("no NetworkManager: {e}"))).ok(),
            owl: None,
            ble: None,
            guard: None,
            borrow: args.mode == Mode::Borrow,
            touched: false,
            borrow_mac: None,
            mac_warned: false,
            timing: Timing::new(),
            vht80: false,
            channel: 0,
            prefer_seen: None,
            prefer_sent: None,
            no_peer_since: None,
        })
    }

    /// The short bring-up (default): no fixed settle waits and no second opinion on the
    /// channel. `--careful` restores the old sequence, where only `--fast` shortened it.
    fn quick(&self) -> bool {
        !self.args.careful || self.args.fast
    }

    /// Record a change: update the state, write the file.
    fn record(&mut self, f: impl FnOnce(&mut State)) {
        f(&mut self.state);
        self.state.save();
    }

    fn enter(&mut self, phase: Phase) {
        log(&format!("phase {:?} -> {phase:?}", self.state.phase));
        self.record(|s| s.phase = phase);
    }

    fn interrupted(&self) -> Result<()> {
        if TERMINATE.load(Ordering::Relaxed) { Err(Stop::Interrupted) } else { Ok(()) }
    }

    /// Sleep, but wake for SIGTERM: setup is interruptible, and teardown still runs.
    fn pause(&self, d: Duration) -> Result<()> {
        let end = Instant::now() + d;
        loop {
            self.interrupted()?;
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(Duration::from_millis(50)));
        }
    }

    // ---- radio ---------------------------------------------------------------

    /// mt76 runtime PM / deep sleep silently stop monitor RX: turn them off.
    fn power_save_off(&mut self, phy: &str) {
        if !sys::is_mount(DEBUGFS) && sys::mount_debugfs().is_ok() && sys::is_mount(DEBUGFS) {
            self.record(|s| s.debugfs_mounted = true);
        }
        let Some(dir) = radio::mt76_dir(phy) else { return };
        for knob in radio::PM_KNOBS {
            let path = format!("{dir}/{knob}");
            let Some(old) = sys::read_int(&path) else { continue };
            self.record(|s| {
                s.pm.entry(path.clone()).or_insert(old);
            });
            if let Err(e) = sys::write_str(&path, "0") {
                log(&format!("cannot disable {knob}: {e}"));
            }
        }
    }

    fn add_vif(&mut self, wiphy: u32, name: &str, mac: &[u8; 6], active: bool) -> Result<()> {
        radio::add_monitor(&mut self.nl, wiphy, name, active).map_err(|e| format!("add {name}: {e}"))?;
        self.record(|s| s.vifs.push(name.to_string()));
        radio::bring_up(name, mac).map_err(|e| Stop::Failed(format!("{name}: set MAC and bring up: {e}")))
    }

    /// The pair: plain vif first (owns the channel), active vif beside it, both with `mac`:
    /// it decides whose channel context mac80211 uses for injected frames (the station's in
    /// shared mode, the GO's in borrow mode).
    fn build_radio(&mut self, phy: &str, wiphy: u32, channel: u32, mac: [u8; 6], strict: bool) -> Result<&'static str> {
        self.add_vif(wiphy, MON_IFACE, &mac, false)?;
        self.power_save_off(phy);
        // In borrow mode the GO's channel is confirmed by nl80211 and silence is only a
        // warning: the quick bring-up does not wait to hear frames.
        let check = strict || !self.quick();
        if check && !radio::receives(MON_IFACE, Duration::from_secs(3)) {
            let msg = format!("the monitor interface hears nothing on channel {channel}");
            if strict {
                return Err(msg.into());
            }
            warn(&msg);
        }
        if self.args.radio == RadioLayout::Plain {
            return Ok(MON_IFACE);
        }
        match self.add_vif(wiphy, ACT_IFACE, &mac, true) {
            Ok(()) => {
                self.power_save_off(phy); // creating a vif can reset the knobs
                Ok(ACT_IFACE)
            }
            Err(Stop::Interrupted) => Err(Stop::Interrupted),
            Err(Stop::Failed(e)) => {
                warn(&format!("no active monitor interface ({e}); the iPhone's frames will not be \
                               acknowledged, transfers will likely fail"));
                Ok(MON_IFACE)
            }
        }
    }

    fn check_injection(&self, vif: &str, channel: u32) {
        if self.args.no_inject_check {
            return;
        }
        // The vif's own address as transmitter: mac80211 picks the channel context of an
        // injected frame from it, as for OWL's frames.
        let mac = sys::mac_of(vif).or_else(|| sys::mac_of(&self.args.iface)).and_then(|m| sys::parse_mac(&m));
        let ok = mac.and_then(|m| radio::injection_works(vif, MON_IFACE, &m));
        let kernel = sys::kernel_release();
        emit("inject_check", json!({ "ok": ok, "kernel": kernel }));
        if ok == Some(false) {
            // A warning, not an error: a false alarm must not stop a session that works.
            warn(&format!("injection looks broken on kernel {kernel}: frames sent on {vif} (channel {channel}) are \
                           not leaving the driver, so the iPhone will not answer. Check `iw reg get` (No-IR) and \
                           the kernel's mt76/mac80211 version"));
        }
    }

    /// E-TXAGG (`--tx-agg`): switch the driver's Block Ack handling for AWDL peers on and
    /// name the interface OWL registers them on. The old value goes into the state file with
    /// the power-saving knobs, so teardown and `recover` put it back. None (with one warning)
    /// when this session cannot do it; OWL then sends as before.
    fn tx_agg(&mut self) -> Option<String> {
        if !self.args.tx_agg {
            return None;
        }
        if !self.borrow || !sys::iface_exists(GO_IFACE) {
            warn("--tx-agg needs borrow mode (the stations live on its access-point interface); sending without aggregation");
            return None;
        }
        if !owl::agg_driver_loaded() {
            // The client asks for it on every send (API 11): not having the build is normal.
            log("tx-agg: the loaded mt7921e has no awdl_ba parameter (scripts/mt7921-ratefix.sh load); sending single frames");
            return None;
        }
        let path = owl::AGG_DRIVER_PARAM.to_string();
        let old = std::fs::read_to_string(&path).is_ok_and(|v| matches!(v.trim(), "Y" | "y" | "1"));
        if let Err(e) = sys::write_str(&path, "1") {
            warn(&format!("--tx-agg: cannot switch {path} on ({e}); sending without aggregation"));
            return None;
        }
        self.record(|s| {
            s.pm.entry(path).or_insert(i64::from(old));
        });
        if let Some(bw) = self.args.tx_agg_bw {
            let path = format!("{}/awdl_bw", owl::AGG_DRIVER_PARAM.rsplit_once('/').map_or("", |p| p.0));
            match sys::read_int(&path) {
                Some(old) => {
                    self.record(|s| {
                        s.pm.entry(path.clone()).or_insert(old);
                    });
                    if let Err(e) = sys::write_str(&path, &bw.to_string()) {
                        warn(&format!("--tx-agg-bw: cannot write {path} ({e}); the firmware picks the width"));
                    }
                }
                None => warn("--tx-agg-bw: this driver build has no awdl_bw parameter; the firmware picks the width"),
            }
        }
        Some(GO_IFACE.to_string())
    }

    /// What the stations looked like at the end of an E-TXAGG session, for the log: rates and
    /// retries as the firmware reports them, and mac80211's view of each Block Ack session.
    fn log_stations(&self) {
        if !self.args.tx_agg || !sys::iface_exists(GO_IFACE) {
            return;
        }
        if let Some((_, out)) = sys::run(sys::tool("iw"), &["dev", GO_IFACE, "station", "dump"]) {
            for line in out.lines().filter(|l| {
                ["Station", "tx packets", "tx retries", "tx failed", "tx bitrate", "rx bitrate", "signal:"].iter().any(|k| l.contains(k))
            }) {
                log(&format!("station: {}", line.trim()));
            }
        }
        let Some(phy) = sys::phy_of(GO_IFACE) else { return };
        let dir = format!("{DEBUGFS}/ieee80211/{phy}/netdev:{GO_IFACE}/stations");
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // What mac80211 made of the capabilities OWL registered (the width above all).
            for file in ["ht_capa", "vht_capa"] {
                if let Ok(text) = std::fs::read_to_string(entry.path().join(file)) {
                    let text: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                    log(&format!("station {name} {file}: {}", text.join("; ")));
                }
            }
            let Ok(text) = std::fs::read_to_string(entry.path().join("agg_status")) else { continue };
            // Rows are `TID RX DTKN SSN TX DTKN pending`: the head, and the TIDs with a session.
            let in_use = |row: &str| {
                let f: Vec<&str> = row.split_whitespace().collect();
                !f.first().is_some_and(|tid| tid.bytes().all(|b| b.is_ascii_digit()))
                    || f.get(1).is_some_and(|rx| *rx != "0")
                    || f.get(4).is_some_and(|tx| *tx != "0")
            };
            for line in text.lines().filter(|l| in_use(l)) {
                log(&format!("station {name} agg: {}", line.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        }
    }

    fn start_owl(&mut self, vif: &str, channel: u32) -> Result<()> {
        self.check_injection(vif, channel);
        let path = owl::find()
            .ok_or_else(|| format!("OWL not found (expected root-owned binary in {})", sys::OWL_PATHS.join(", ")))?;
        let binary = io(owl::Binary::open(path))?;
        let a = self.args;
        let sta_iface = self.tx_agg();
        let settings = owl::Settings {
            vif: vif.to_string(),
            channel,
            borrow: self.borrow,
            intersect: a.intersect,
            static_seq: a.static_seq,
            dps_single_band: a.dps_single_band,
            dps_infra_channel: a.dps_infra_channel,
            narrow: a.narrow,
            vht80: self.vht80,
            realtime: a.realtime,
            tx_rate: a.tx_rate.clone(),
            ampdu: a.ampdu,
            no_ampdu: a.no_ampdu,
            presence: a.presence,
            election_metric: a.election_metric,
            sta_iface,
            ba_probe: a.ba_probe,
            verbose: a.verbose,
        };
        let owl = Owl::start(&owl::command_line(&binary, &settings), self.quick())?;
        let pid = owl.pid();
        self.owl = Some(owl);
        self.record(|s| s.owl_pid = Some(pid));
        self.enter(Phase::Running);
        Ok(())
    }

    fn no_ir(&mut self, wiphy: u32, channel: u32) -> bool {
        self.nl.freq_is_no_ir(wiphy, sys::freq_of_channel(channel)).unwrap_or(false)
    }

    // ---- scan guard ------------------------------------------------------------

    /// Borrow mode, P2.7: while the station is disconnected something keeps scanning
    /// (sweeps back to back, 2026-11-11), and each scanned channel takes the radio off the
    /// AWDL channel for ~130 ms out of ~240. Stop every scan on our phy as it starts, whoever
    /// asked for it. Ends in `down()`, before Wi-Fi reconnects (that needs its own scan).
    fn start_scan_guard(&mut self, wiphy: u32) {
        let (mut events, mut aborter) = match (ScanEvents::open(), Nl80211::open()) {
            (Ok(e), Ok(a)) => (e, a),
            (Err(e), _) | (_, Err(e)) => {
                warn(&format!("scan guard not started ({e}): Wi-Fi scans will slow the transfer"));
                return;
            }
        };
        log("scan guard: stopping Wi-Fi scans while AirDrop has the radio");
        // One may be running already (NetworkManager's after the disconnect ran 9 s into the
        // session on 2026-11-12); "none running" is not an error.
        if aborter.abort_scan(&self.args.iface).unwrap_or(false) {
            log("scan guard: stopped the scan that was already running");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicU32::new(0));
        let (stop_t, stopped_t) = (stop.clone(), stopped.clone());
        let iface = self.args.iface.clone();
        let thread = std::thread::spawn(move || {
            let mut errors = 0u32;
            while !stop_t.load(Ordering::Relaxed) {
                let batch = match events.poll() {
                    Ok(b) => b,
                    Err(e) => {
                        // Events overflowed (ENOBUFS) or similar: we may have missed a scan
                        // start, so stop whatever runs now and keep listening.
                        errors += 1;
                        if errors <= 5 {
                            log(&format!("scan guard: event socket: {e}"));
                        }
                        let _ = aborter.abort_scan(&iface);
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                };
                for ev in batch.iter().filter(|e| e.wiphy == wiphy) {
                    let res = aborter.stop_scan(ev);
                    if stopped_t.fetch_add(1, Ordering::Relaxed) < 5 {
                        let what = if ev.scheduled { "scheduled scan" } else { "scan" };
                        log(&format!("scan guard: {what} on {:?} -> {res:?}", ev.target));
                    }
                }
            }
        });
        self.guard = Some(ScanGuard { stop, stopped, thread });
    }

    fn stop_scan_guard(&mut self) {
        if let Some(g) = self.guard.take() {
            g.stop.store(true, Ordering::Relaxed);
            let _ = g.thread.join();
            log(&format!("scan guard: stopped {} scan(s)", g.stopped.load(Ordering::Relaxed)));
        }
    }

    // ---- modes -----------------------------------------------------------------

    /// Wi-Fi stays connected; AWDL shares its channel through the monitor pair.
    fn shared(&mut self, phy: &str, wiphy: u32, ap_channel: Option<u32>) -> Result<u32> {
        let iface = &self.args.iface;
        let ap = ap_channel
            .ok_or_else(|| format!("{iface} is not connected to Wi-Fi; shared mode borrows its channel (connect to Wi-Fi first)"))?;
        if !AWDL_CHANNELS.contains(&ap) {
            return Err(format!("your Wi-Fi is on channel {ap}; AirDrop can share it only on {} (change the router's channel)",
                AWDL_CHANNELS.map(|c| c.to_string()).join(", ")).into());
        }
        if let Ok(requested) = self.args.channel.parse::<u32>()
            && requested != ap {
            return Err(format!("channel {requested} requested, but Wi-Fi is on {ap} and shared mode cannot leave it").into());
        }
        if self.no_ir(wiphy, ap) {
            warn(&format!("channel {ap} is No-IR in the current regulatory domain: the kernel will silently drop \
                           everything AirDrop sends. Fix: sudo iw reg set <country, e.g. DE>"));
        }
        let mac = sys::mac_of(iface).and_then(|m| sys::parse_mac(&m)).ok_or("cannot read the station's MAC")?;
        let vif = self.build_radio(phy, wiphy, ap, mac, true)?;
        self.timing.mark("monitor pair");
        self.start_owl(vif, ap)?;
        self.timing.mark("owl started");
        Ok(ap)
    }

    /// What must hold before Wi-Fi is paused, so a refusal costs the user nothing.
    fn borrow_preflight(&mut self, wiphy: u32, channel: u32) -> Result<()> {
        if !GO_CHANNELS.contains(&channel) {
            return Err(format!("borrow mode runs on channel {}", GO_CHANNELS.map(|c| c.to_string()).join(", ")).into());
        }
        if sys::iface_exists(GO_IFACE) {
            return Err(format!("{GO_IFACE} already exists (run: airdrop recover)").into());
        }
        if self.no_ir(wiphy, channel) {
            return Err(format!("channel {channel} is No-IR in the current regulatory domain (fix: sudo iw reg set <country, e.g. DE>)").into());
        }
        Ok(())
    }

    /// Ask NetworkManager to let go of the radio; what it had is recorded first, so a
    /// crash right after still reconnects.
    fn pause_wifi(&mut self, channel: u32) -> Result<()> {
        let iface = self.args.iface.clone();
        // NetworkManager must not randomize wlan0's MAC for scans while disconnected.
        self.record(|s| s.nm_borrow_conf = true);
        io(nm::write_conf(self.nm.as_ref(), nm::BORROW_CONF, &format!(
            "# written by airdrop-helper, removed on teardown\n[device-airdrop-borrow]\n\
             match-device=interface-name:{iface}\nwifi.scan-rand-mac-address=no\n")))?;
        let active = self.nm.as_ref().and_then(|nm| nm.active_connection(&iface).ok().flatten());
        if let Some((uuid, name)) = active {
            let ap = self.nm.as_ref().and_then(|nm| nm.active_ap(&iface));
            self.record(|s| s.nm_disconnected = Some(Paused { uuid: Some(uuid), name: Some(name.clone()), autoconnect_only: false, ap }));
            notice(&format!("Wi-Fi '{name}' is paused while AirDrop runs on channel {channel}; it reconnects when AirDrop stops"));
            let nm = self.nm.as_ref().ok_or("no NetworkManager")?;
            nm.disconnect(&iface).map_err(|e| format!("could not pause Wi-Fi: {e}"))?;
            for _ in 0..20 {
                if !self.nl.associated(&iface).unwrap_or(false) {
                    break;
                }
                self.pause(Duration::from_millis(250))?;
            }
        } else if self.nl.associated(&iface).unwrap_or(false) {
            return Err("Wi-Fi is connected but not through NetworkManager; borrow mode only pauses NetworkManager connections".into());
        } else if self.nm.is_some() {
            // Not connected: keep NetworkManager from auto-connecting mid-session (that would
            // pull the radio off `channel`); teardown re-enables it.
            self.record(|s| s.nm_disconnected = Some(Paused { uuid: None, name: None, autoconnect_only: true, ap: None }));
            if let Some(Err(e)) = self.nm.as_ref().map(|nm| nm.set_autoconnect(&iface, false)) {
                log(&format!("nm: autoconnect off failed: {e}"));
            }
        }
        Ok(())
    }

    /// Wi-Fi paused for this session only: NetworkManager disconnects the station (it stays up
    /// and managed), AWDL takes the radio on `channel`, and teardown reconnects the same
    /// connection. Nothing here can leave wlan0 unmanaged or down.
    fn borrow(&mut self, phy: &str, wiphy: u32, channel: u32) -> Result<u32> {
        self.borrow_preflight(wiphy, channel)?;
        self.enter(Phase::Borrowed);
        self.pause_wifi(channel)?;
        self.timing.mark("wi-fi paused");
        // Default: stop scans from here on. NetworkManager's scan after the disconnect is what
        // the old settle wait and frame check were waiting out (P2.7; 2026-11-11 campus).
        let guard_first = !self.args.careful && !self.args.no_scan_guard;
        if guard_first {
            self.start_scan_guard(wiphy);
        }
        self.borrow_mac = sys::mac_of(&self.args.iface);
        let want = sys::freq_of_channel(channel);
        // E0.11: under remain-on-channel nothing we inject reaches the iPhone (wlan0's firmware
        // BSS is inactive while disconnected). A P2P-GO on `channel` is the radio's only active
        // context: the firmware sits there and has an active BSS to send through (our vifs carry
        // the GO's MAC, so mac80211 injects via the GO).
        // A short settle: the regulatory domain can change once the access point's country hint
        // is gone (checked below). 500 ms is what the listener has used live since 2026-11-11;
        // the scan is already stopped by the guard, and the GO start retries while busy.
        // --careful (API 8): 2 s for NetworkManager's scan to finish, 500 ms with --fast.
        let settle = if self.args.careful && !self.args.fast { 2000 } else { 500 };
        self.pause(Duration::from_millis(settle))?;
        self.timing.mark("settle wait");
        if self.no_ir(wiphy, channel) {
            // The regulatory domain can change when the access point's country hint goes away.
            return Err(format!("channel {channel} became No-IR after Wi-Fi was paused (fix: sudo iw reg set <country, e.g. DE>)").into());
        }
        if self.nm.is_some() {
            self.record(|s| s.nm_go_conf = true);
            io(nm::write_conf(self.nm.as_ref(), nm::GO_CONF, &format!(
                "# written by airdrop-helper, removed on teardown\n[keyfile]\nunmanaged-devices=interface-name:{GO_IFACE}\n")))?;
        }
        self.record(|s| s.go = true);
        let quick = self.quick();
        let vht80 = !self.args.ht40 && (self.args.vht80 || (149..=161).contains(&channel));
        let (mac, wide80) = radio::start_go(&mut self.nl, wiphy, channel, self.args.narrow, vht80, quick, self.args.tx_agg && owl::agg_driver_loaded())?;
        self.vht80 = wide80;
        self.record(|s| s.go_mac = Some(radio::mac_string(&mac)));
        self.timing.mark("access point up");
        let vif = self.build_radio(phy, wiphy, channel, mac, false)?;
        self.timing.mark("monitor pair");
        if self.quick() {
            // The GO's channel is already confirmed by nl80211; listening for frames as a
            // second opinion costs 1-9 s.
        } else {
            let mut heard = None;
            for attempt in 0..4 {
                self.pause(Duration::from_secs(if attempt == 0 { 1 } else { 2 }))?;
                // Right after the disconnect NetworkManager still scans; where many access points
                // are around most frames in a sample come from the scan's other channels
                // (2462 MHz x19 against 5745 MHz x1 failed a session). Hearing ours at all is enough.
                heard = radio::heard_frequency(MON_IFACE, Some(want));
                if heard == Some(want) {
                    break;
                }
            }
            match heard {
                Some(f) if f == want => log(&format!("radio confirmed on {f} MHz ({vif})")),
                Some(_) => warn(&format!("heard nothing on channel {channel} yet (only other channels, from a Wi-Fi \
                                          scan); the hidden access point is on it, continuing")),
                None => warn(&format!("could not confirm the radio is on channel {channel} (no frames heard yet); continuing")),
            }
        }
        self.timing.mark("channel check");
        if !guard_first && !self.args.no_scan_guard {
            self.start_scan_guard(wiphy);
        }
        self.start_owl(vif, channel)?;
        self.timing.mark("owl started");
        Ok(channel)
    }

    // ---- extras ---------------------------------------------------------------

    /// systemd-resolved would run its own mDNS/LLMNR on awdl0; the link is ours. Per-link
    /// and gone with the interface, so nothing to restore.
    fn quiet_resolved(&self) {
        for proto in ["mdns", "llmnr"] {
            sys::run(sys::tool("resolvectl"), &[proto, AWDL_IFACE, "no"]);
        }
    }

    /// P2.1: BBR on the awdl0 link-local route only. No measurable gain, so opt-in. The
    /// route disappears with awdl0: nothing to restore.
    fn tune_tcp(&self) {
        sys::run(sys::tool("modprobe"), &["tcp_bbr"]);
        let ok = sys::run(sys::tool("ip"), &["-6", "route", "replace", "fe80::/64", "dev", AWDL_IFACE,
            "metric", "256", "proto", "kernel", "congctl", "bbr"]).is_some_and(|(ok, _)| ok);
        if !ok {
            warn("BBR unavailable for awdl0; using the default congestion control");
        }
    }

    fn start_ble(&mut self) {
        let version = if self.args.ble_version == BleVersion::V1 { ble::Version::V1 } else { ble::Version::V3 };
        match Advertiser::start(version) {
            Ok(adv) => {
                self.ble = Some(adv);
                self.record(|s| s.ble = true);
            }
            Err(e) => warn(&format!("BLE advertisement failed ({e}); iPhones will not switch AWDL on")),
        }
    }

    // ---- lifecycle -------------------------------------------------------------------

    pub fn up(&mut self) -> Result<()> {
        let a = self.args;
        let phy = sys::phy_of(&a.iface).ok_or_else(|| format!("{} is not a wireless interface", a.iface))?;
        let wiphy = sys::phy_index(&phy).ok_or("cannot read the phy index")?;
        self.timing = Timing::new();
        self.touched = true;
        if let Some(stale) = State::load() {
            log("found state from a previous session, restoring it first");
            restore(&stale);
            State::clear();
        }
        self.state.save();
        // read before anything changes
        let ap_channel = self.nl.interface(&a.iface).ok().and_then(|i| i.freq).and_then(sys::channel_of_freq);
        self.interrupted()?;
        let channel = if self.borrow {
            // Default 149: a sending iPhone on 5 GHz Wi-Fi keeps its AirDrop data path on the
            // 5 GHz social channel (46 slots on 149 vs 28 on ch 6), and 11.5 MB completed there
            // in 17.6 s vs resets on ch 6 (E0.11 run 8). --channel 6 for a 2.4 GHz-infra iPhone.
            self.borrow(&phy, wiphy, a.channel.parse().unwrap_or(149))?
        } else {
            self.shared(&phy, wiphy, ap_channel)?
        };
        self.channel = channel;
        self.interrupted()?;
        // The advert first: it is what makes an iPhone switch AWDL on.
        if a.ble && a.ble_version != BleVersion::Off {
            self.start_ble();
        }
        self.quiet_resolved();
        if a.bbr {
            self.tune_tcp();
        }
        self.timing.mark("resolved, ble");
        self.timing.report("up");
        emit("up", json!({
            "api": HELPER_API, "iface": AWDL_IFACE, "mode": if self.borrow { "borrow" } else { "shared" },
            "channel": channel, "ble": self.state.ble, "active": self.state.vifs.iter().any(|v| v == ACT_IFACE),
            "wifi_channel": ap_channel, "second_channel": null,
        }));
        Ok(())
    }

    /// Fail-safe teardown, run exactly once: every part runs whatever the others do.
    pub fn down(&mut self) {
        if !std::mem::take(&mut self.touched) {
            return;
        }
        self.enter(Phase::TearingDown);
        let started = Instant::now();
        self.stop_scan_guard(); // first: reconnecting Wi-Fi needs a scan
        self.log_stations();
        if let Some(mut owl) = self.owl.take() {
            owl.stop();
        }
        if let Some(adv) = self.ble.take() {
            adv.stop();
        }
        log(&format!("timing: scan guard, owl and ble stopped in {} ms", started.elapsed().as_millis()));
        restore(&self.state);
        State::clear();
        self.state = State::new(&self.args.iface);
    }

    /// Tell the client when a peer would be better met on another channel (EXPERIMENTS C-2,
    /// E2.11). An iPhone joined to a 5 GHz network on 36 gives a social channel 2 of its 16
    /// slots and its own channel 7, and mostly does not answer us on the social one. The
    /// client decides whether to move; this only reports, once per channel, after the same
    /// answer on two checks in a row (a transfer on our channel changes the peer's sequence
    /// to mostly ours, which ends the wish by itself).
    fn channel_hint(&mut self) {
        const HOME: u32 = 149;
        let view = owl::peer_view();
        let count = |seq: &[u32], c: u32| seq.iter().filter(|&&s| s == c).count();
        let mut want = None;
        if view.present {
            self.no_peer_since = None;
            if let Some(seq) = &view.sequence {
                let ours = count(seq, self.channel);
                want = GO_CHANNELS.iter().copied().filter(|&c| c != self.channel)
                    .map(|c| (count(seq, c), c)).filter(|&(n, _)| n >= 4 && n > ours).max().map(|(_, c)| c);
            }
        } else if self.channel != HOME {
            // Off the social channel we cannot hear an iPhone that is not on this network.
            let since = *self.no_peer_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(15) {
                want = Some(HOME);
            }
        }
        let Some(c) = want else {
            self.prefer_seen = None;
            return;
        };
        let n = match self.prefer_seen {
            Some((seen, n)) if seen == c => n + 1,
            _ => 1,
        };
        self.prefer_seen = Some((c, n));
        if n >= 2 && self.prefer_sent != Some(c) {
            self.prefer_sent = Some(c);
            log(&format!("channel hint: a peer is better met on channel {c} (we are on {})", self.channel));
            emit("prefer", json!({ "channel": c }));
        }
    }

    /// Block until stdin closes (the client is gone, however it died), OWL dies, SIGTERM,
    /// or in borrow mode Wi-Fi reconnects behind our back (then stop, and leave it connected).
    /// Returns false when the session ended on a failure (it was reported as `error`).
    pub fn wait(&mut self) -> bool {
        let mut next_check = Instant::now() + Duration::from_secs(2);
        while !TERMINATE.load(Ordering::Relaxed) {
            if let Some(reason) = self.owl.as_mut().and_then(Owl::died) {
                emit("error", json!({ "message": reason }));
                return false;
            }
            if stdin_closed(1000) {
                return true;
            }
            if self.borrow && Instant::now() >= next_check {
                next_check = Instant::now() + Duration::from_secs(2);
                let iface = &self.args.iface;
                let now_mac = sys::mac_of(iface);
                if now_mac != self.borrow_mac && !self.mac_warned {
                    self.mac_warned = true;
                    warn(&format!("{iface} changed its MAC to {} (it was {}); our frames may not be sent",
                        now_mac.unwrap_or_default(), self.borrow_mac.clone().unwrap_or_default()));
                }
                if self.nl.associated(iface).unwrap_or(false) {
                    emit("error", json!({ "message": "Wi-Fi reconnected while AirDrop had the radio; stopping AirDrop (Wi-Fi stays connected)" }));
                    return false;
                }
                self.channel_hint();
            }
        }
        true
    }
}

/// True when stdin reached EOF (or errored) within `ms`; data is read and dropped.
fn stdin_closed(ms: i32) -> bool {
    let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    // SAFETY: pfd is a valid pollfd and we pass a count of 1.
    if unsafe { libc::poll(&mut pfd, 1, ms) } <= 0 {
        return false; // timeout or EINTR (SIGTERM): the caller re-checks its flags
    }
    if pfd.revents & libc::POLLIN != 0 {
        let mut buf = [0u8; 4096];
        // SAFETY: buf is writable for its length.
        return unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) } == 0;
    }
    pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}
