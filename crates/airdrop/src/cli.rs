//! The commands. Each returns an exit code; Ctrl-C / SIGTERM make it stop, run its own
//! cleanup, and end with 130.

use crate::config::{config_path, Channel, Config, Media, Mode};
use crate::dump::Dump;
use crate::prompt::{Decision, Prompter};
use crate::receiver::{Receiver, Settings};
use crate::sender::{Peer, SendError, Sender, What};
use crate::stop::{Interrupted, Stop};
use crate::util::{env, env_secs, expand_user};
use airdrop_proto::wire;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const EXIT_DECLINED: i32 = 3;
pub const EXIT_NOT_FOUND: i32 = 4;
pub const EXIT_STOPPED: i32 = 130;

#[derive(Parser)]
#[command(name = "airdrop", version, about = "AirDrop for Linux")]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Args)]
pub struct Global {
    /// Debug output
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Device name shown to others
    #[arg(long, global = true)]
    pub name: Option<String>,
    /// Wi-Fi interface for AWDL (default from config: wlan0)
    #[arg(long, global = true)]
    pub iface: Option<String>,
    /// auto (default): borrow, or shared when Wi-Fi is already on channel 149. borrow: Wi-Fi
    /// is paused for this session, AirDrop runs on channel 149 and Wi-Fi reconnects
    /// afterwards. shared: Wi-Fi stays connected (only reaches an iPhone on your Wi-Fi's channel)
    #[arg(long, global = true, value_parser = ["auto", "shared", "borrow"])]
    pub mode: Option<String>,
    /// AWDL channel (auto: 149 in borrow mode; 6 for an iPhone whose Wi-Fi is on 2.4 GHz or off)
    #[arg(long, global = true, value_parser = ["auto", "6", "36", "44", "149"])]
    pub channel: Option<String>,
    /// Events as JSON lines on stdout, commands as JSON lines on stdin (for a desktop shell;
    /// README "Machine interface"). Implies --no-open; with --ask, questions are answered on stdin
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum PrompterKind {
    Terminal,
    Notify,
    /// `ask` events answered on stdin (what --json uses)
    Stdin,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// List nearby AirDrop receivers
    Find {
        /// Record the protocol (requests, replies, TXT records) as JSON in DIR
        #[arg(long, value_name = "DIR")]
        dump: Option<PathBuf>,
        #[arg(short, long, default_value_t = 30.0)]
        timeout: f64,
    },
    /// Send files, folders or a link
    Send {
        /// Files and folders to send
        paths: Vec<String>,
        /// Send a link instead of files
        #[arg(long)]
        url: Option<String>,
        /// Device name (or part of it) to send to
        #[arg(short, long)]
        receiver: Option<String>,
        /// Seconds to search
        #[arg(short, long, default_value_t = 60.0)]
        timeout: f64,
        #[arg(long, value_name = "DIR")]
        dump: Option<PathBuf>,
        /// Skip AWDL and send to HOST:PORT (testing)
        #[arg(long, hide = true)]
        peer: Option<String>,
    },
    /// Be visible and accept AirDrops (everything, unless --ask)
    Receive {
        /// Ask before accepting ([y]es / [n]o / [a]lways)
        #[arg(long)]
        ask: bool,
        /// Accept everything without asking (the default)
        #[arg(long)]
        accept_all: bool,
        /// With --ask: where to ask
        #[arg(long, value_enum, default_value = "terminal")]
        prompter: PrompterKind,
        /// With --ask: seconds to wait for an answer before declining
        #[arg(long, value_name = "SECONDS", default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
        ask_timeout: u64,
        /// Never open received links
        #[arg(long)]
        no_open: bool,
        /// Do not keep a Bluetooth scan running while receiving
        #[arg(long)]
        no_ble_scan: bool,
        /// Save every request an Apple device sends, for protocol analysis
        #[arg(long, value_name = "DIR")]
        dump: Option<PathBuf>,
        /// Where files are saved
        #[arg(long)]
        download_dir: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Listen on --bind without AWDL (testing)
        #[arg(long, hide = true)]
        no_link: bool,
        #[arg(long, hide = true, default_value = "::1")]
        bind: String,
    },
    /// Stay in the background with Wi-Fi connected; when an iPhone nearby opens AirDrop,
    /// pause Wi-Fi for the transfer, save, and reconnect
    Listen {
        /// Only report what is heard on Bluetooth; Wi-Fi is never touched
        #[arg(long)]
        dry_run: bool,
        /// React only to adverts carrying this 2-byte contact hash (repeatable)
        #[arg(long, value_name = "HEX4")]
        hash: Vec<String>,
        /// Ask before saving; default: accept everything
        #[arg(long)]
        ask: bool,
        #[arg(long, hide = true)]
        accept_all: bool,
        #[arg(long, value_enum, default_value = "terminal")]
        prompter: PrompterKind,
        /// With --ask: seconds to wait for an answer before declining
        #[arg(long, value_name = "SECONDS", default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
        ask_timeout: u64,
        #[arg(long)]
        no_open: bool,
        #[arg(long)]
        download_dir: Option<String>,
    },
    /// Check that this machine can AirDrop
    Doctor,
    /// Undo a crashed AirDrop session and give Wi-Fi back to NetworkManager
    Recover,
    /// Show or change settings
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    Show,
    Set { key: String, value: String },
    /// Senders accepted automatically
    Known {
        #[arg(value_parser = ["list", "add", "rm"])]
        op: String,
        sender: Option<String>,
    },
}

fn apply_overrides(cfg: &mut Config, g: &Global, download_dir: Option<&String>) -> Result<(), String> {
    if let Some(n) = &g.name {
        cfg.name.clone_from(n);
    }
    if let Some(i) = &g.iface {
        cfg.wifi_iface.clone_from(i);
    }
    if let Some(m) = &g.mode {
        cfg.mode = Mode::parse(m)?;
    }
    if let Some(c) = &g.channel {
        cfg.channel = Channel::parse(c)?;
    }
    if let Some(d) = download_dir {
        cfg.download_dir = expand_user(d);
    }
    Ok(())
}

pub async fn run(args: Cli) -> i32 {
    let download_dir = match &args.cmd {
        Cmd::Receive { download_dir, .. } | Cmd::Listen { download_dir, .. } => download_dir.as_ref(),
        _ => None,
    };
    let mut cfg = match Config::load().and_then(|mut c| apply_overrides(&mut c, &args.global, download_dir).map(|()| c)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            if args.global.json {
                crate::events::enable();
                crate::events::error("config", &e);
            }
            return 2;
        }
    };
    let stop = Stop::install();
    let json = args.global.json;
    if json && !matches!(args.cmd, Cmd::Config { .. } | Cmd::Doctor | Cmd::Recover) {
        crate::events::start(&stop);
    }
    let result = match args.cmd {
        Cmd::Config { action } => return config_cmd(&mut cfg, action),
        Cmd::Send { timeout, dump, peer, .. } if json => send_json(cfg, &stop, timeout, dump, peer).await,
        Cmd::Send { paths, url, receiver, timeout, dump, peer } => send(cfg, &stop, paths, url, receiver, timeout, dump, peer).await,
        Cmd::Receive { ask, accept_all, prompter, ask_timeout, no_open, no_ble_scan, dump, port, no_link, bind, .. } => {
            let prompter = make_prompter(ask && !accept_all, if json { PrompterKind::Stdin } else { prompter }, ask_timeout);
            receive(cfg, &stop, prompter, !no_open && !json, no_ble_scan, dump, port, no_link, &bind).await
        }
        Cmd::Find { dump, timeout } => crate::link_cmds::find(cfg, &stop, dump, timeout).await,
        Cmd::Listen { dry_run, hash, ask, prompter, ask_timeout, no_open, .. } => {
            let prompter = make_prompter(ask, if json { PrompterKind::Stdin } else { prompter }, ask_timeout);
            crate::link_cmds::listen(cfg, &stop, dry_run, hash, prompter, !no_open && !json).await
        }
        Cmd::Doctor => Ok(crate::link_cmds::doctor(&cfg)),
        Cmd::Recover => crate::link_cmds::recover(&cfg).await,
    };
    match result {
        Ok(code) => code,
        Err(Interrupted) => {
            eprintln!("\nStopped.");
            EXIT_STOPPED
        }
    }
}

fn make_prompter(ask: bool, kind: PrompterKind, timeout_s: u64) -> Prompter {
    let timeout = Duration::from_secs(timeout_s);
    match (ask, kind) {
        (false, _) => Prompter::Static(Decision::Accept),
        (true, PrompterKind::Terminal) => Prompter::Terminal { timeout },
        (true, PrompterKind::Notify) => Prompter::Notification { timeout },
        (true, PrompterKind::Stdin) => Prompter::Stdin { timeout },
    }
}

/// `ReceiverMediaCapabilities` for our /Discover reply (experiment E3.4):
/// `AIRDROP_MEDIA_CAPS` unset = by the `media` setting; `none` = left out; a value starting
/// with `{` = that JSON as-is; anything else = a file whose bytes are sent.
fn media_caps(media: Media) -> Result<Option<Vec<u8>>, String> {
    match env("AIRDROP_MEDIA_CAPS") {
        None => Ok(Some(match media {
            Media::Compatible => wire::DEFAULT_MEDIA_CAPS.to_vec(),
            Media::Original => wire::ORIGINAL_MEDIA_CAPS.to_vec(),
        })),
        Some(v) if v == "none" => Ok(None),
        Some(v) if v.starts_with('{') => {
            serde_json::from_str::<serde_json::Value>(&v).map_err(|e| format!("AIRDROP_MEDIA_CAPS is not valid JSON: {e}"))?;
            Ok(Some(v.into_bytes()))
        }
        Some(path) => std::fs::read(expand_user(&path)).map(Some).map_err(|e| format!("AIRDROP_MEDIA_CAPS {path}: {e}")),
    }
}

pub fn receiver_settings(cfg: &Config, open_urls: bool) -> Result<Settings, String> {
    let ask_status = match env("AIRDROP_ASK_STATUS") {
        None => 200,
        Some(v) => v.parse::<u16>().ok().filter(|s| (100..600).contains(s)).ok_or_else(|| format!("AIRDROP_ASK_STATUS={v:?} is not an HTTP status"))?,
    };
    let unknown_status = match env("AIRDROP_UNKNOWN_STATUS") {
        None => 400,
        Some(v) => v.parse::<u16>().ok().filter(|s| (100..600).contains(s)).ok_or_else(|| format!("AIRDROP_UNKNOWN_STATUS={v:?} is not an HTTP status"))?,
    };
    Ok(Settings {
        media_caps: media_caps(cfg.media)?,
        contact_exchange: env("AIRDROP_CONTACT_EXCHANGE").as_deref() == Some("1"),
        ask_status,
        ask_delay: env_secs("AIRDROP_ASK_DELAY", 0.0),
        unknown_status,
        stall: stall_timeout(),
        open_urls,
    })
}

/// No byte moved for this long = the other side is gone (locked, walked away).
pub fn stall_timeout() -> Duration {
    env_secs("AIRDROP_STALL_TIMEOUT", 60.0).max(Duration::from_secs(1))
}

pub fn open_dump(dir: Option<PathBuf>) -> Result<Option<Dump>, i32> {
    let Some(dir) = dir else { return Ok(None) };
    match Dump::new(&dir) {
        Ok(d) => {
            eprintln!("Recording protocol dumps in {}", d.dir().display());
            Ok(Some(d))
        }
        Err(e) => {
            eprintln!("--dump {}: {e}", dir.display());
            Err(1)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive(cfg: Config, stop: &Stop, prompter: Prompter, open_urls: bool, no_ble_scan: bool, dump: Option<PathBuf>,
                 port: Option<u16>, no_link: bool, bind: &str) -> Result<i32, Interrupted> {
    let settings = match receiver_settings(&cfg, open_urls) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("config error: {e}");
            return Ok(2);
        }
    };
    let dump = match open_dump(dump) {
        Ok(d) => d,
        Err(code) => return Ok(code),
    };
    let acceptor = match crate::tls::acceptor(&cfg.name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("TLS identity: {e}");
            crate::events::error("start", &format!("TLS identity: {e}"));
            return Ok(1);
        }
    };
    if !no_link {
        return crate::link_cmds::receive(cfg, stop, settings, prompter, acceptor, dump, port, no_ble_scan).await;
    }
    let Ok(ip) = bind.parse::<IpAddr>() else {
        eprintln!("--bind {bind}: not an IP address");
        return Ok(2);
    };
    let name = cfg.name.clone();
    let dir = cfg.download_dir.display().to_string();
    let (rx, port) = match Receiver::start(SocketAddr::new(ip, port.unwrap_or(0)), cfg, settings, prompter, acceptor, dump.map(std::sync::Arc::new)).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot listen on [{bind}]:{}: {e}", port.unwrap_or(0));
            crate::events::error("start", &format!("cannot listen on port {}: {e}", port.unwrap_or(0)));
            return Ok(1);
        }
    };
    crate::say!("Listening on [{bind}]:{port} as '{}' (no AWDL).", crate::util::clean(&name));
    crate::events::emit("visible", serde_json::json!({ "name": crate::util::clean(&name), "dir": dir, "port": port }));
    stop.wait().await;
    rx.close().await;
    Err(Interrupted)
}

pub fn make_sender(cfg: &Config, dump: Option<Dump>) -> Result<Sender, String> {
    Ok(Sender {
        tls: crate::tls::connector(&cfg.name).map_err(|e| format!("TLS identity: {e}"))?,
        name: cfg.name.clone(),
        model: cfg.model.clone(),
        id: cfg.service_id.clone(),
        stall: stall_timeout(),
        dump: dump.map(Arc::new),
    })
}

/// Send to one peer and turn the outcome into an exit code.
pub async fn deliver(sender: &Sender, peer: &mut Peer, what: &What, stop: &Stop) -> Result<i32, Interrupted> {
    let id = crate::events::next_id();
    match stop.guard(sender.send(peer, what, id)).await {
        Ok(result) => Ok(report_sent(id, peer, result)),
        Err(Interrupted) => {
            crate::events::emit("transfer", serde_json::json!({ "id": id, "dir": "out", "peer": peer.host(), "phase": "cancelled" }));
            Err(Interrupted)
        }
    }
}

/// Say how a send ended (and emit its last `transfer` event); returns the exit code.
pub fn report_sent(id: u64, peer: &Peer, result: Result<(), SendError>) -> i32 {
    let (phase, message, code) = match result {
        Ok(()) => ("done", String::new(), 0),
        Err(SendError::Declined(m)) => {
            eprintln!("{m}");
            ("declined", m, EXIT_DECLINED)
        }
        Err(SendError::Failed(m)) => {
            eprintln!("Send failed: {m}");
            ("failed", m, 1)
        }
    };
    crate::events::emit("transfer", serde_json::json!({ "id": id, "dir": "out", "peer": peer.host(), "phase": phase, "message": message }));
    code
}

/// `[v6]:port`, `[fe80::1%awdl0]:port` or `host:port`.
fn parse_peer(text: &str) -> Option<(SocketAddr, Option<String>)> {
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (h, p) = rest.split_once("]:")?;
        (h, p)
    } else {
        text.rsplit_once(':')?
    };
    let port: u16 = port.parse().ok()?;
    if let Some((ip, iface)) = host.split_once('%') {
        let ip: std::net::Ipv6Addr = ip.parse().ok()?;
        return Some((crate::sender::scoped(ip, port, iface), Some(iface.to_string())));
    }
    Some((SocketAddr::new(host.parse().ok()?, port), None))
}

#[allow(clippy::too_many_arguments)]
async fn send(cfg: Config, stop: &Stop, paths: Vec<String>, url: Option<String>, receiver: Option<String>, timeout: f64,
              dump: Option<PathBuf>, peer: Option<String>) -> Result<i32, Interrupted> {
    let paths: Vec<PathBuf> = paths.iter().map(|p| expand_user(p)).collect();
    if paths.is_empty() == url.is_none() {
        eprintln!("Give either files/folders or --url.");
        return Ok(2);
    }
    let mut resolved = vec![];
    for p in paths {
        if std::fs::symlink_metadata(&p).is_err() {
            eprintln!("{}: no such file or directory", p.display());
            return Ok(2);
        }
        // ".", ".." or "dir/" have no usable name of their own
        resolved.push(if p.file_name().is_none() { p.canonicalize().unwrap_or(p) } else { p });
    }
    let what = match url {
        Some(u) => What::Link(u),
        None => What::Files(resolved),
    };
    let dump = match open_dump(dump) {
        Ok(d) => d,
        Err(code) => return Ok(code),
    };
    let sender = match make_sender(&cfg, dump) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Ok(1);
        }
    };
    if let Some(text) = peer {
        let Some((addr, iface)) = parse_peer(&text) else {
            eprintln!("--peer: use HOST:PORT or [IPv6]:PORT");
            return Ok(2);
        };
        return deliver(&sender, &mut Peer::new(addr, iface), &what, stop).await;
    }
    crate::link_cmds::send(cfg, stop, sender, what, receiver, timeout).await
}

/// `send --json`: the payload comes with each `pick` on stdin (see `link_cmds::send_json`).
async fn send_json(cfg: Config, stop: &Stop, timeout: f64, dump: Option<PathBuf>, peer: Option<String>) -> Result<i32, Interrupted> {
    let dump = match open_dump(dump) {
        Ok(d) => d,
        Err(code) => return Ok(code),
    };
    let sender = match make_sender(&cfg, dump) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            crate::events::error("start", &e);
            return Ok(1);
        }
    };
    let fixed = match peer {
        None => None,
        Some(text) => {
            let Some((addr, iface)) = parse_peer(&text) else {
                eprintln!("--peer: use HOST:PORT or [IPv6]:PORT");
                return Ok(2);
            };
            Some(Peer::new(addr, iface))
        }
    };
    crate::link_cmds::send_json(cfg, stop, sender, timeout, fixed).await
}

fn config_cmd(cfg: &mut Config, action: ConfigAction) -> i32 {
    match action {
        ConfigAction::Show if crate::events::on() => {
            crate::events::emit("config", serde_json::json!({
                "name": cfg.name, "dir": cfg.download_dir, "known": cfg.known_senders, "mode": cfg.mode.as_str(),
                "channel": cfg.channel.to_string(), "iface": cfg.wifi_iface, "path": config_path(),
            }));
            0
        }
        ConfigAction::Show => {
            match std::fs::read_to_string(config_path()) {
                Ok(t) => print!("{t}"),
                Err(_) => println!("(defaults)"),
            }
            0
        }
        ConfigAction::Set { key, value } => {
            let result: Result<(), String> = match key.as_str() {
                "name" => {
                    cfg.name = value;
                    Ok(())
                }
                "model" => {
                    cfg.model = value;
                    Ok(())
                }
                "download_dir" => {
                    cfg.download_dir = expand_user(&value);
                    Ok(())
                }
                "wifi_iface" => {
                    cfg.wifi_iface = value;
                    Ok(())
                }
                "mode" => Mode::parse(&value).map(|m| cfg.mode = m),
                "channel" => Channel::parse(&value).map(|c| cfg.channel = c),
                "port" => value.parse().map(|p| cfg.port = p).map_err(|_| format!("port must be a number, got {value:?}")),
                "media" => match value.as_str() {
                    "compatible" => {
                        cfg.media = Media::Compatible;
                        Ok(())
                    }
                    "original" => {
                        cfg.media = Media::Original;
                        Ok(())
                    }
                    _ => Err(format!("media must be compatible or original, got {value:?}")),
                },
                _ => {
                    eprintln!("unknown or read-only key {key:?}");
                    return 2;
                }
            };
            if let Err(e) = result {
                eprintln!("{e}");
                return 2;
            }
            match cfg.save() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{}: {e}", config_path().display());
                    1
                }
            }
        }
        ConfigAction::Known { op, sender } => {
            if op != "list" && sender.is_none() {
                eprintln!("give a sender name");
                return 2;
            }
            let sender = sender.unwrap_or_default();
            let result = match op.as_str() {
                "list" => {
                    println!("{}", if cfg.known_senders.is_empty() { "(none)".into() } else { cfg.known_senders.join("\n") });
                    Ok(true)
                }
                "add" => cfg.add_known(&sender).map(|()| true),
                _ => cfg.remove_known(&sender),
            };
            match result {
                Ok(true) => 0,
                Ok(false) => {
                    eprintln!("{sender:?} is not in the list");
                    1
                }
                Err(e) => {
                    eprintln!("{}: {e}", config_path().display());
                    1
                }
            }
        }
    }
}
