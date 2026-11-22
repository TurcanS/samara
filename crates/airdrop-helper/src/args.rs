//! Command line. Same flags as the Python helper, so the existing client can drive it.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "airdrop-helper", version, about = "Root half of the Linux AirDrop client (run via pkexec)")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Bring AWDL up; it runs until stdin closes or SIGTERM, then restores everything.
    Up(Box<Up>),
    /// Undo whatever a crashed session left behind and hand Wi-Fi back.
    Recover(IfaceArg),
    /// Read-only facts for `airdrop doctor`.
    Probe(IfaceArg),
    /// Print the helper API level (no privileges needed).
    Api,
}

#[derive(Args)]
pub struct IfaceArg {
    #[arg(long, value_parser = iface_name)]
    pub iface: String,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum Mode {
    /// Accepted for older clients: means shared.
    Auto,
    /// Wi-Fi stays up; AWDL uses its channel.
    Shared,
    /// Wi-Fi paused for the session; AWDL takes the radio on a social channel.
    Borrow,
    /// Removed: it took Wi-Fi down. Refused.
    Exclusive,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum BleVersion {
    #[value(name = "3")]
    V3,
    #[value(name = "1")]
    V1,
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum BorrowRadio {
    Go,
    /// Removed experiment (E0.9). Refused.
    Roc,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum RadioLayout {
    Pair,
    Plain,
}

#[derive(Args)]
pub struct Up {
    #[arg(long, value_parser = iface_name)]
    pub iface: String,
    #[arg(long, value_enum, default_value = "shared")]
    pub mode: Mode,
    #[arg(long, default_value = "auto", value_parser = ["auto", "6", "36", "44", "149"])]
    pub channel: String,
    #[arg(long)]
    pub ble: bool,
    /// Removed: a second channel beside a connected station was refuted on this chip.
    #[arg(long, hide = true, value_parser = ["6", "44", "149"])]
    pub go_channel: Option<String>,
    #[arg(long, hide = true, value_enum, default_value = "pair")]
    pub radio: RadioLayout,
    #[arg(long, hide = true)]
    pub static_seq: bool,
    #[arg(long, hide = true)]
    pub intersect: bool,
    #[arg(long, value_enum, default_value = "3")]
    pub ble_version: BleVersion,
    #[arg(long, hide = true, value_enum, default_value = "go")]
    pub borrow_radio: BorrowRadio,
    #[arg(long)]
    pub dps_single_band: bool,
    #[arg(long)]
    pub dps_infra_channel: bool,
    /// Accepted, no effect (HT40 is the default in borrow mode).
    #[arg(long, hide = true)]
    pub wide: bool,
    #[arg(long)]
    pub narrow: bool,
    /// 80 MHz (GO) + VHT caps (OWL -V) also on channels 36-48; on 149-161 it is the
    /// borrow default since E-VHT80 (2026-11-14).
    #[arg(long, hide = true, conflicts_with_all = ["narrow", "ht40"])]
    pub vht80: bool,
    /// No 80 MHz: the GO at 40 MHz and HT caps only (API 9 behaviour).
    #[arg(long, hide = true, conflicts_with = "narrow")]
    pub ht40: bool,
    #[arg(long)]
    pub realtime: bool,
    /// Experimental: BBR on the awdl0 route (no measurable gain, off by default).
    #[arg(long, hide = true)]
    pub bbr: bool,
    #[arg(long, hide = true, value_parser = clap::value_parser!(u32).range(1..=1000), value_name = "1-1000")]
    pub election_metric: Option<u32>,
    #[arg(long, value_parser = tx_rate, value_name = "6|9|12|18|24|36|48|54|auto|auto-ht|mcsN[-40][-sgi]")]
    pub tx_rate: Option<String>,
    /// Aggregated sending (E-TXAGG; the client passes it for `send` since API 11): OWL
    /// registers the peer it uploads to as a station of the GO (`-G`) and asks it for a Block
    /// Ack session, the GO beacons its HT/VHT elements, and the firmware picks the rate and
    /// aggregates. Borrow mode with the mt7921e build that has the `awdl_ba` parameter (it is
    /// switched on for the session); without either the session sends single frames as before.
    #[arg(long)]
    pub tx_agg: bool,
    /// With --tx-agg: fix the width of aggregated frames (1 = 40 MHz, 2 = 80 MHz) through the
    /// driver's `awdl_bw` parameter; without it the firmware decides (20 MHz on 2026-11-16).
    #[arg(long, hide = true, requires = "tx_agg", value_parser = clap::value_parser!(u32).range(1..=2), value_name = "1|2")]
    pub tx_agg_bw: Option<u32>,
    /// Register peers as stations of the GO without asking them for Block Ack (OWL `-g`), so the
    /// chip has an address entry for the iPhone while frames keep OWL's radiotap rate. Borrow mode
    /// only. Automatic on rtw89 (driver.rs); this flag forces it on any driver (experiments).
    #[arg(long, hide = true)]
    pub tx_sta: bool,
    /// Experiment: OWL asks the peer for a Block Ack session itself (`-B`) and logs the answer.
    #[arg(long, hide = true)]
    pub ba_probe: bool,
    #[arg(long)]
    pub ampdu: bool,
    #[arg(long)]
    pub no_ampdu: bool,
    /// Accepted for older clients: the short bring-up is the default since HELPER_API 9.
    #[arg(long, hide = true)]
    pub fast: bool,
    /// The bring-up before HELPER_API 9: fixed settle waits and a frame check on the channel
    /// (with --fast, the listener's shorter waits); the scan guard starts after the radio.
    #[arg(long, hide = true)]
    pub careful: bool,
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=100), value_name = "0-100")]
    pub presence: Option<u32>,
    /// Accepted for older command lines: the scan guard is the default in borrow mode.
    #[arg(long)]
    pub scan_guard: bool,
    #[arg(long)]
    pub no_scan_guard: bool,
    #[arg(long)]
    pub no_inject_check: bool,
    #[arg(short, long)]
    pub verbose: bool,
}

fn iface_name(s: &str) -> Result<String, String> {
    let ok = (1..=15).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok { Ok(s.to_string()) } else { Err("invalid interface name".into()) }
}

fn tx_rate(s: &str) -> Result<String, String> {
    let plain = ["6", "9", "12", "18", "24", "36", "48", "54", "auto", "auto-ht"].contains(&s);
    let mcs = s.strip_prefix("mcs").is_some_and(|rest| {
        let rest = rest.strip_suffix("-sgi").unwrap_or(rest);
        let rest = rest.strip_suffix("-40").unwrap_or(rest);
        matches!(rest.parse::<u8>(), Ok(n) if n <= 15 && n.to_string() == rest)
    });
    if plain || mcs { Ok(s.to_string()) } else { Err("use 6, 9, 12, 18, 24, 36, 48, 54 or mcs0..mcs15[-40][-sgi]".into()) }
}
