//! The commands that use the radio: find, send and receive over AWDL, listen, doctor,
//! recover. Each brings the link up, does its work, and takes everything down again in
//! order (mDNS goodbye, receiver, helper teardown with Wi-Fi reconnect) whatever ended it.

use crate::ble;
use crate::cli::{deliver, open_dump, receiver_settings, EXIT_NOT_FOUND};
use crate::config::{config_path, Channel, Config, Mode};
use crate::dump::Dump;
use crate::link::{self, Link, LinkError, Options};
use crate::mdns::Mdns;
use crate::prompt::{stdin_lines, Prompter};
use crate::receiver::{Receiver, Settings};
use crate::sender::{scoped, txt_flags, Peer, Sender, What};
use crate::stop::{Interrupted, Stop};
use crate::util::{clean, clock, which};
use airdrop_proto::wire::APPLE_PORT;
use openssl::ssl::SslAcceptor;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::Ipv6Addr;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

/// A link error as an exit code (or the stop it was).
fn link_failed(e: LinkError) -> Result<i32, Interrupted> {
    match e {
        LinkError::Interrupted => Err(Interrupted),
        LinkError::Failed(m) => {
            eprintln!("AWDL: {m}");
            Ok(1)
        }
    }
}

// ---- search (find, send) -------------------------------------------------------------

/// What a search saw, so a failed one can say why. Measured 2026-11-08: in every failed
/// send the iPhone's radio acknowledged our frames but its AirDrop never answered,
/// because "Everyone for 10 Minutes" had run out.
#[derive(Default)]
struct Stats {
    radio_peers: HashSet<Ipv6Addr>,
    answered: usize,
    channel: String,
}

impl Stats {
    fn hint(&self) -> String {
        if self.answered > 0 {
            return String::new();
        }
        if !self.radio_peers.is_empty() {
            let n = self.radio_peers.len();
            return format!("  {n} Apple device{} in radio range but did not answer. On the iPhone: Settings > General > AirDrop > \
                Everyone for 10 Minutes (it switches itself off after 10 minutes, so set it again), screen on and unlocked. Then retry.",
                if n == 1 { " is" } else { "s are" });
        }
        let other = if self.channel == "149" { "--channel 6 if the iPhone's Wi-Fi is on 2.4 GHz or off" } else { "--mode borrow to use AirDrop's own channel 149" };
        format!("  No Apple device was heard on channel {}. On the iPhone: Wi-Fi and Bluetooth on, screen on and unlocked, AirDrop set to \
            Everyone for 10 Minutes. If it still is not found, try {other}.", self.channel)
    }
}

/// mDNS browse plus owly DISC-5: an iPhone's mDNS often never reaches awdl0, so every AWDL
/// neighbour also gets a /Discover on Apple's port directly (a receiver refuses until the
/// BLE advert has woken it, so this keeps trying). Each answering device arrives once.
fn search(link: &Link, mdns: &mut Mdns, cfg: &Config, sender: Arc<Sender>, dump: Option<Arc<Dump>>, stats: Arc<Mutex<Stats>>)
    -> (mpsc::Receiver<Peer>, JoinSet<()>) {
    let (tx, rx) = mpsc::channel(32);
    let mut tasks = JoinSet::new();
    let reported = Arc::new(Mutex::new(HashSet::<Ipv6Addr>::new()));
    let busy = Arc::new(Mutex::new(HashSet::<Ipv6Addr>::new()));
    let iface = link.iface.clone();
    // One /Discover per device at a time; report it once.
    let check = {
        let (reported, busy, stats, sender) = (reported.clone(), busy.clone(), stats.clone(), sender.clone());
        move |ip: Ipv6Addr, port: u16, service: String, iface: String, tx: mpsc::Sender<Peer>| {
            let (reported, busy, stats, sender) = (reported.clone(), busy.clone(), stats.clone(), sender.clone());
            async move {
                if reported.lock().expect("not poisoned").contains(&ip) || !busy.lock().expect("not poisoned").insert(ip) {
                    return false;
                }
                let mut peer = Peer::new(scoped(ip, port, &iface), Some(iface));
                peer.service = service;
                let ok = sender.discover(&mut peer).await;
                busy.lock().expect("not poisoned").remove(&ip);
                if let Err(e) = ok {
                    crate::verbose!("receiver at [{ip}] did not answer /Discover: {e}");
                    return false;
                }
                if reported.lock().expect("not poisoned").insert(ip) {
                    stats.lock().expect("not poisoned").answered += 1;
                    let _ = tx.send(peer).await;
                }
                true
            }
        }
    };
    let mut found = mdns.browse(&cfg.service_id);
    {
        let (check, tx, iface) = (check.clone(), tx.clone(), iface.clone());
        tasks.spawn(async move {
            while let Some(f) = found.recv().await {
                if let Some(d) = &dump {
                    d.service(&f.name, &format!("{}%{iface}", f.addr), f.port, &f.txt);
                }
                tokio::spawn(check(f.addr, f.port, f.name, iface.clone(), tx.clone()));
            }
        });
    }
    let own = link.address;
    tasks.spawn(async move {
        let mut inflight: HashMap<Ipv6Addr, tokio::task::JoinHandle<()>> = HashMap::new();
        loop {
            let i = iface.clone();
            let neighbours = tokio::task::spawn_blocking(move || link::awdl_neighbors(&i)).await.unwrap_or_default();
            for addr in neighbours.into_iter().filter(|a| *a != own) {
                stats.lock().expect("not poisoned").radio_peers.insert(addr);
                if reported.lock().expect("not poisoned").contains(&addr) || inflight.get(&addr).is_some_and(|t| !t.is_finished()) {
                    continue;
                }
                let (check, tx, iface) = (check.clone(), tx.clone(), iface.clone());
                inflight.insert(addr, tokio::spawn(async move {
                    if !check(addr, APPLE_PORT, String::new(), iface, tx).await {
                        tokio::time::sleep(Duration::from_secs(3)).await; // then a later poll tries again
                    }
                }));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    (rx, tasks)
}

struct Session {
    link: Link,
    mdns: Mdns,
}

impl Session {
    async fn up(cfg: &Config, stop: &Stop, opts: Options) -> Result<Self, LinkError> {
        let link = Link::up(cfg, opts, stop).await?;
        match Mdns::open(&link.iface) {
            Ok(mdns) => Ok(Self { link, mdns }),
            Err(e) => {
                link.close().await;
                Err(LinkError::Failed(format!("mDNS on awdl0: {e}")))
            }
        }
    }

    async fn close(self) {
        self.mdns.close().await;
        self.link.close().await;
    }
}

pub async fn find(cfg: Config, stop: &Stop, dump: Option<PathBuf>, timeout: f64) -> Result<i32, Interrupted> {
    let dump = match open_dump(dump) {
        Ok(d) => d.map(Arc::new),
        Err(code) => return Ok(code),
    };
    let sender = match crate::cli::make_sender(&cfg, None) {
        Ok(mut s) => {
            s.dump = dump.clone();
            Arc::new(s)
        }
        Err(e) => {
            eprintln!("{e}");
            return Ok(1);
        }
    };
    eprintln!("Looking for AirDrop devices for {timeout:.0}s (set the iPhone to 'Everyone for 10 minutes', screen on)...");
    let mut s = match Session::up(&cfg, stop, Options { ble: true, fast: false, quiet: false, sending: false, channel: None }).await {
        Ok(s) => s,
        Err(e) => return link_failed(e),
    };
    let stats = Arc::new(Mutex::new(Stats { channel: s.link.channel.clone(), ..Stats::default() }));
    let (mut peers, mut tasks) = search(&s.link, &mut s.mdns, &cfg, sender, dump, stats.clone());
    let mut found = 0;
    let end = tokio::time::sleep(Duration::from_secs_f64(timeout.max(0.0)));
    tokio::pin!(end);
    let outcome = loop {
        tokio::select! {
            Some(p) = peers.recv() => {
                found += 1;
                let state = if p.available() { "available" } else { "contacts only (not reachable)" };
                println!("{}\t{}\t{state}\t[{}]:{}", p.name.as_deref().map_or("(unnamed)".into(), clean),
                    p.model.as_deref().map_or("-".into(), clean), p.host(), p.addr.port());
            }
            () = &mut end => break Ok(()),
            reason = s.link.failed() => break Err(reason),
            () = stop.wait() => break Ok(()),
        }
    };
    tasks.abort_all();
    s.close().await;
    if stop.requested() {
        return Err(Interrupted);
    }
    if let Err(reason) = outcome {
        eprintln!("AWDL: {reason}");
        return Ok(1);
    }
    if found == 0 {
        eprintln!("No devices found.");
        eprintln!("{}", stats.lock().expect("not poisoned").hint());
        return Ok(EXIT_NOT_FOUND);
    }
    Ok(0)
}

pub async fn send(cfg: Config, stop: &Stop, sender: Sender, what: What, receiver: Option<String>, timeout: f64) -> Result<i32, Interrupted> {
    let sender = Arc::new(sender);
    let want = receiver.as_deref().map(str::to_lowercase);
    match &receiver {
        Some(r) => eprintln!("Looking for '{}' (up to {timeout:.0}s)...", clean(r)),
        None => eprintln!("Looking for devices. Type a number and press Enter to send:"),
    }
    let mut s = match Session::up(&cfg, stop, Options { ble: true, fast: false, quiet: false, sending: true, channel: None }).await {
        Ok(s) => s,
        Err(e) => return link_failed(e),
    };
    let stats = Arc::new(Mutex::new(Stats { channel: s.link.channel.clone(), ..Stats::default() }));
    let (mut found, mut tasks) = search(&s.link, &mut s.mdns, &cfg, sender.clone(), sender.dump.clone(), stats.clone());
    let mut listed: Vec<Peer> = vec![];
    let mut hidden: HashSet<String> = HashSet::new();
    let end = tokio::time::sleep(Duration::from_secs_f64(timeout.max(0.0)));
    tokio::pin!(end);
    let mut lines = if want.is_none() { Some(stdin_lines().lock().await) } else { None };
    // The timeout bounds only the search; the transfer runs with the link still up.
    let chosen: Result<Option<Peer>, String> = loop {
        tokio::select! {
            Some(p) = found.recv() => {
                if !p.available() {
                    if hidden.insert(p.host()) {
                        eprintln!("  found {} at [{}], but it is not accepting from everyone: on the iPhone set AirDrop to \
                                   'Everyone for 10 Minutes' (it switches itself off after 10 minutes)",
                                  p.model.as_deref().map_or("a device".into(), clean), p.host());
                    }
                    continue;
                }
                match &want {
                    Some(w) if p.name.as_deref().is_some_and(|n| n.to_lowercase().contains(w.as_str())) => break Ok(Some(p)),
                    Some(_) => {}
                    None => {
                        listed.push(p);
                        let p = listed.last().expect("just pushed");
                        // The "type a number" line has scrolled away under the bring-up by now.
                        eprintln!("  [{n}] {} ({})   <- type {n} and press Enter to send", p.label(),
                                  p.model.as_deref().map_or("unknown".into(), clean), n = listed.len());
                    }
                }
            }
            Some(Some(line)) = async { match lines.as_mut() { Some(l) => l.recv().await, None => std::future::pending().await } } => {
                let text = line.trim();
                match text.parse::<usize>() {
                    Ok(n) if (1..=listed.len()).contains(&n) => break Ok(Some(listed[n - 1].clone())),
                    _ if !text.is_empty() => eprintln!("Type a number between 1 and {} and press Enter.", listed.len()),
                    _ => {}
                }
            }
            () = &mut end => break Ok(None),
            reason = s.link.failed() => break Err(reason),
            () = stop.wait() => break Ok(None),
        }
    };
    drop(lines);
    tasks.abort_all();
    let code = match chosen {
        _ if stop.requested() => Err(Interrupted),
        Err(reason) => {
            eprintln!("AWDL: {reason}");
            Ok(1)
        }
        Ok(None) => {
            eprintln!("{}", if want.is_some() { "No matching device found." } else { "Nothing selected." });
            let hint = stats.lock().expect("not poisoned").hint();
            if listed.is_empty() && hidden.is_empty() && !hint.is_empty() {
                eprintln!("{hint}");
            }
            Ok(EXIT_NOT_FOUND)
        }
        Ok(Some(mut peer)) => {
            let link = &s.link;
            tokio::select! {
                r = deliver(&sender, &mut peer, &what, stop) => r,
                reason = link.failed() => {
                    eprintln!("Send failed: AWDL stopped ({reason})");
                    Ok(1)
                }
            }
        }
    };
    s.close().await;
    code
}

// ---- receive ---------------------------------------------------------------------------

/// Keep a Bluetooth LE scan running while we receive (unprivileged, Wi-Fi untouched).
/// Every flow that took a file on the first tap had one (`listen`, the trace script);
/// plain `receive` without it left the iPhone on "Waiting..." (2026-11-12, cause not
/// proven: `--no-ble-scan` is the control run).
fn keep_le_scan() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut adverts = ble::watch();
        while let Some(a) = adverts.recv().await {
            if let Err(e) = a {
                eprintln!("No Bluetooth scan ({e}); receiving continues, but the iPhone may need more than one tap.");
                return;
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn receive(cfg: Config, stop: &Stop, settings: Settings, prompter: Prompter, acceptor: SslAcceptor, dump: Option<Dump>,
                     port: Option<u16>, no_ble_scan: bool) -> Result<i32, Interrupted> {
    /// A request this recent means an iPhone is talking to us where we are: stay.
    const QUIET_BEFORE_MOVE: Duration = Duration::from_secs(8);
    /// Not more often than this, so two iPhones in different states cannot make us flap.
    const BETWEEN_MOVES: Duration = Duration::from_secs(20);
    // Opt-in (`AIRDROP_CHANNEL_MOVE=1`) until a run shows it helps: the sessions it was built on
    // turned out to fail for another reason (EXPERIMENTS C-3). Never with a channel or shared
    // mode pinned by the user.
    let may_move = cfg.channel == Channel::Auto && cfg.mode != Mode::Shared && crate::util::env("AIRDROP_CHANNEL_MOVE").as_deref() == Some("1");
    let dump = dump.map(Arc::new);
    let scan = (!no_ble_scan).then(keep_le_scan);
    let mut channel: Option<u32> = None;
    let mut moved_at: Option<Instant> = None;
    let mut announced = false;
    let code = loop {
        let opts = Options { ble: false, fast: channel.is_some(), quiet: channel.is_some(), sending: false, channel };
        let mut s = match Session::up(&cfg, stop, opts).await {
            Ok(s) => s,
            Err(e) => break link_failed(e),
        };
        let (name, dir, service_id) = (cfg.name.clone(), cfg.download_dir.clone(), cfg.service_id.clone());
        let addr = scoped(s.link.address, port.unwrap_or(cfg.port), &s.link.iface);
        let (rx, port) = match Receiver::start(addr, cfg.clone(), settings.clone(), prompter.clone(), acceptor.clone(), dump.clone()).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("cannot listen on [{}%{}]:{}: {e}", s.link.address, s.link.iface, addr.port());
                s.close().await;
                break Ok(1);
            }
        };
        s.mdns.register(&service_id, port, s.link.address, txt_flags()).await;
        if !announced {
            announced = true;
            println!("Visible as '{}'. Files go to {}. Press Ctrl-C to stop.", clean(&name), dir.display());
        }
        /// Why this link ended.
        enum End {
            Stop,
            Failed,
            Move(u32),
        }
        let wanted = tokio::select! {
            () = stop.wait() => None,
            _ = s.link.failed() => Some(Err(())), // the helper said why ("AWDL: ...")
            c = s.link.prefers(), if may_move => Some(Ok(c)),
        };
        if matches!(wanted, Some(Ok(_))) {
            // Wait for a quiet moment: never in a transfer, never right after a request.
            while !stop.requested() {
                let activity = rx.activity();
                let wait = if activity.busy() {
                    Some(Duration::from_secs(2))
                } else {
                    let quiet = QUIET_BEFORE_MOVE.checked_sub(activity.last().elapsed());
                    let apart = moved_at.and_then(|t| BETWEEN_MOVES.checked_sub(t.elapsed()));
                    quiet.max(apart)
                };
                let Some(wait) = wait else { break };
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = stop.wait() => {}
                }
            }
        }
        let end = match wanted {
            Some(Ok(c)) if !stop.requested() => End::Move(c),
            Some(Err(())) => End::Failed,
            _ => End::Stop,
        };
        rx.close().await;
        match end {
            End::Stop => {
                s.close().await;
                break Err(Interrupted);
            }
            End::Failed => {
                s.close().await;
                break Ok(1);
            }
            End::Move(c) => {
                if c == 149 {
                    eprintln!("No iPhone left on channel {}: back to AirDrop channel 149.", s.link.channel);
                } else {
                    eprintln!("An iPhone nearby is on a Wi-Fi network on channel {c}: moving AirDrop there (a few seconds).");
                }
                s.close().await;
                channel = (c != 149).then_some(c);
                moved_at = Some(Instant::now());
            }
        }
    };
    if let Some(t) = scan {
        t.abort();
    }
    code
}

// ---- listen ---------------------------------------------------------------------------

/// An advert must persist this long (a 0.7 s blip was seen when the sheet only flashed).
const DEBOUNCE: Duration = Duration::from_secs(1);
/// Without adverts this long, the share sheet was closed.
const EPISODE_GAP: Duration = Duration::from_secs(8);
/// Sheet closed and nothing from the iPhone for this long: give Wi-Fi back.
const NO_CONTACT: Duration = Duration::from_secs(25);
/// Without any request this long, even if the sheet stays open.
const MAX_IDLE: Duration = Duration::from_secs(90);
/// After a saved transfer, before Wi-Fi comes back.
const AFTER_SAVE: Duration = Duration::from_millis(1500);

/// One opening of a share sheet, per iPhone (keyed by its hashes, which outlive its
/// rotating address): a second iPhone opening its sheet during another's episode counts.
struct Episode {
    first: Instant,
    last: Instant,
    served: bool,
    adverts: u32,
}

type Adverts = mpsc::Receiver<Result<ble::Advert, String>>;

fn hash_list(hashes: &BTreeSet<String>) -> String {
    hashes.iter().cloned().collect::<Vec<_>>().join(" ")
}

/// The share sheets open nearby, as the Bluetooth watcher hears them.
struct Sheets {
    adverts: Adverts,
    wanted: BTreeSet<String>,
    episodes: HashMap<BTreeSet<String>, Episode>,
    /// The last wanted advert; `None` before the first.
    last_advert: Option<Instant>,
    /// The watcher stopped while a session was up; the next wait reports it.
    ended: bool,
}

impl Sheets {
    /// Collect adverts until a sheet has been open long enough (`Continue`), or the
    /// watcher fails (`Break` with the exit code).
    async fn wait_for_opening(&mut self, stop: &Stop) -> Result<ControlFlow<i32>, Interrupted> {
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        loop {
            tokio::select! {
                a = self.adverts.recv() => match a {
                    Some(Ok(a)) if a.matches(&self.wanted) => {
                        match self.episodes.get_mut(&a.hashes) {
                            Some(ep) if a.when.duration_since(ep.last) <= EPISODE_GAP => ep.last = a.when,
                            _ => { self.episodes.insert(a.hashes, Episode { first: a.when, last: a.when, served: false, adverts: 1 }); }
                        }
                        self.last_advert = Some(a.when);
                        continue;
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => { eprintln!("Bluetooth: {e}"); return Ok(ControlFlow::Break(1)) }
                    None => { eprintln!("Bluetooth: the scan stopped"); return Ok(ControlFlow::Break(1)) }
                },
                _ = tick.tick() => {}
                () = stop.wait() => return Err(Interrupted),
            }
            let now = Instant::now();
            self.episodes.retain(|_, ep| now - ep.last < Duration::from_secs(60));
            let ready = self.episodes.values().any(|ep| !ep.served && now - ep.last < DEBOUNCE && ep.last - ep.first >= DEBOUNCE);
            if ready {
                // One session per opening: every sheet open now is served by this session.
                for ep in self.episodes.values_mut() {
                    ep.served = true;
                }
                return Ok(ControlFlow::Continue(()));
            }
        }
    }

    /// While a session is up: only keep track of whether a wanted sheet is still open.
    async fn follow(&mut self) {
        match self.adverts.recv().await {
            Some(Ok(a)) if a.matches(&self.wanted) => self.last_advert = Some(a.when),
            Some(_) => {}
            // without this the closed channel would wake the session loop at once, forever
            None => self.ended = true,
        }
    }

    fn sheet_open(&self, now: Instant) -> bool {
        self.last_advert.is_some_and(|t| now - t < EPISODE_GAP)
    }
}

pub async fn listen(cfg: Config, stop: &Stop, dry_run: bool, hashes: Vec<String>, prompter: Prompter, open_urls: bool) -> Result<i32, Interrupted> {
    let wanted: BTreeSet<String> = hashes.iter().map(|h| h.to_lowercase()).collect();
    let wifi = link::station_channel(&cfg.wifi_iface).map_or("not connected".into(), |c| format!("connected on channel {c}"));
    let who = if wanted.is_empty() { "any iPhone nearby".to_string() } else { format!("only devices with hash {}", hash_list(&wanted)) };
    let adverts = ble::watch();
    if dry_run {
        println!("Listening on Bluetooth only (Wi-Fi {wifi}, left alone; test mode, nothing is changed). Reacting to {who}. Ctrl-C to stop.");
        return watch_only(adverts, &wanted, stop).await;
    }

    let acceptor = match crate::tls::acceptor(&cfg.name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("TLS identity: {e}");
            return Ok(1);
        }
    };
    let settings = match receiver_settings(&cfg, open_urls) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("config error: {e}");
            return Ok(2);
        }
    };
    println!("Listening for AirDrop as '{}' (Wi-Fi {wifi} and left alone until {who} opens AirDrop; then it is paused for the \
              transfer and reconnected). Files go to {}{}. Ctrl-C to stop.", clean(&cfg.name), cfg.download_dir.display(),
             if prompter.is_interactive() { ", after asking" } else { " without asking (--ask to be asked)" });
    let mut sheets = Sheets { adverts, wanted, episodes: HashMap::new(), last_advert: None, ended: false };
    let served = Served { cfg: &cfg, settings: &settings, prompter: &prompter, acceptor: &acceptor };
    loop {
        if let ControlFlow::Break(code) = sheets.wait_for_opening(stop).await? {
            return Ok(code);
        }
        served.once(stop, &mut sheets).await?;
    }
}

/// `listen --dry-run`: report openings, change nothing.
async fn watch_only(mut adverts: Adverts, wanted: &BTreeSet<String>, stop: &Stop) -> Result<i32, Interrupted> {
    let mut episodes: HashMap<BTreeSet<String>, Episode> = HashMap::new();
    let mut total = 0;
    let code = loop {
        tokio::select! {
            a = adverts.recv() => match a {
                Some(Ok(a)) => {
                    let ep = episodes.get_mut(&a.hashes);
                    match ep {
                        Some(ep) if a.when.duration_since(ep.last) <= EPISODE_GAP => {
                            ep.last = a.when;
                            ep.adverts += 1;
                        }
                        _ => {
                            if let Some(ep) = ep {
                                println!("           ...that one lasted {:.1} s, {} adverts", (ep.last - ep.first).as_secs_f64(), ep.adverts);
                            }
                            total += 1;
                            println!("{}  AirDrop opened nearby: hashes {}{}", clock(), hash_list(&a.hashes),
                                if a.matches(wanted) { "  <- would trigger" } else { "  (ignored: not in the list)" });
                            episodes.insert(a.hashes, Episode { first: a.when, last: a.when, served: false, adverts: 1 });
                        }
                    }
                }
                Some(Err(e)) => { eprintln!("Bluetooth: {e}"); break Ok(1) }
                None => { eprintln!("Bluetooth: the scan stopped"); break Ok(1) }
            },
            () = stop.wait() => break Err(Interrupted),
        }
    };
    for (h, ep) in &episodes {
        println!("last from {}: {:.1} s, {} adverts", hash_list(h), (ep.last - ep.first).as_secs_f64(), ep.adverts);
    }
    println!("{total} opening(s) seen");
    code
}

/// What one listen session needs, read once.
struct Served<'a> {
    cfg: &'a Config,
    settings: &'a Settings,
    prompter: &'a Prompter,
    acceptor: &'a SslAcceptor,
}

impl Served<'_> {
    /// One opening: pause Wi-Fi, be visible, receive, give Wi-Fi back. A failed session
    /// never ends the listener (the helper already restored Wi-Fi); only a stop does.
    async fn once(&self, stop: &Stop, sheets: &mut Sheets) -> Result<(), Interrupted> {
        let cfg = self.cfg;
        let t0 = Instant::now();
        println!("{}  AirDrop opened nearby: pausing Wi-Fi and becoming visible...", clock());
        let quiet = !crate::is_verbose();
        let mut s = match Session::up(cfg, stop, Options { ble: false, fast: true, quiet, sending: false, channel: None }).await {
            Ok(s) => s,
            Err(LinkError::Interrupted) => return Err(Interrupted),
            Err(LinkError::Failed(m)) => {
                eprintln!("AWDL: {m}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                return Ok(());
            }
        };
        let addr = scoped(s.link.address, cfg.port, &s.link.iface);
        let started = Receiver::start(addr, cfg.clone(), self.settings.clone(), self.prompter.clone(), self.acceptor.clone(), None).await;
        let (rx, port) = match started {
            Ok(r) => r,
            Err(e) => {
                eprintln!("cannot listen on [{}%{}]:{}: {e}", s.link.address, s.link.iface, cfg.port);
                s.close().await;
                return Ok(());
            }
        };
        s.mdns.register(&cfg.service_id, port, s.link.address, txt_flags()).await;
        let up = Instant::now();
        println!("{}  Visible as '{}' {:.1} s after the trigger ({} mode, channel {}).", clock(), clean(&cfg.name),
                 (up - t0).as_secs_f64(), s.link.mode, s.link.channel);
        let stopped = serve_until_done(&mut s, &rx, sheets, stop, up).await;
        rx.close().await;
        let t_down = Instant::now();
        s.close().await;
        if stopped {
            return Err(Interrupted);
        }
        let now = Instant::now();
        println!("{}  Wi-Fi is back ({:.1} s to reconnect; paused {:.1} s in total). Listening again.", clock(),
                 (now - t_down).as_secs_f64(), (now - t0).as_secs_f64());
        Ok(())
    }
}

/// Wait while the receiver works; return once Wi-Fi can be given back (true: stopped).
async fn serve_until_done(s: &mut Session, rx: &Receiver, sheets: &mut Sheets, stop: &Stop, up: Instant) -> bool {
    let mut first_request = false;
    loop {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(250)) => {}
            () = sheets.follow(), if !sheets.ended => {}
            _ = s.link.failed() => return false,
            () = stop.wait() => return true,
        }
        let now = Instant::now();
        let act = rx.activity();
        if act.requests() > 0 && !first_request {
            first_request = true;
            println!("{}  The iPhone found us {:.1} s after we became visible.", clock(), (now - up).as_secs_f64());
        }
        if act.busy() {
            continue;
        }
        let idle = now - act.last().max(up);
        if act.completed() > 0 && idle > AFTER_SAVE {
            return false;
        }
        if !sheets.sheet_open(now) && idle > NO_CONTACT {
            println!("{}  Nothing was sent to us.", clock());
            return false;
        }
        if idle > MAX_IDLE {
            println!("{}  Nothing was sent to us for {} s.", clock(), MAX_IDLE.as_secs());
            return false;
        }
    }
}

// ---- recover, doctor -----------------------------------------------------------------

pub async fn recover(cfg: &Config) -> Result<i32, Interrupted> {
    match link::recover(&cfg.wifi_iface).await {
        Ok(steps) => {
            println!("Restored: {}", if steps.is_empty() { "nothing needed".into() } else { steps.join(", ") });
            Ok(0)
        }
        Err(e) => {
            eprintln!("recover: {e}");
            Ok(1)
        }
    }
}

fn run(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// The block of `iw phy P channels` for one frequency (`None`: not listed).
fn channel_block<'a>(chans: &'a str, freq: &str) -> Option<&'a str> {
    let rest = chans.split_once(&format!("* {freq} MHz"))?.1;
    Some(rest.split('*').next().unwrap_or(rest))
}

/// A path as one shell word, for a command the user copies: single-quoted only when needed.
fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn doctor(cfg: &Config) -> i32 {
    let mut ok_all = true;
    let mut row = |ok: Option<bool>, label: &str, detail: &str| {
        let mark = match ok {
            Some(true) => "ok ",
            Some(false) => {
                ok_all = false;
                "ERR"
            }
            None => "-- ",
        };
        println!("[{mark}] {label}{}", if detail.is_empty() { String::new() } else { format!(": {detail}") });
    };
    row(Some(which("pkexec").is_some()), "pkexec (polkit)", "");
    // pkexec runs elsewhere: the install line needs an absolute path, so name the helper
    // built next to this binary when there is one.
    let built = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("airdrop-helper"))).filter(|p| p.is_file())
        .map_or("<source folder>/target/release/airdrop-helper".to_string(), |p| sh_quote(&p.display().to_string()));
    match link::find_helper() {
        Ok(h) => {
            let api = link::helper_api(&h);
            if link::is_script(&h) {
                // the retired Python helper: same API level, but no longer maintained
                row(Some(false), "root helper", &format!("{} is the old Python helper. Install the Rust one: \
                    pkexec install -Dm755 {built} {}", h.display(), link::SYSTEM_HELPER));
            } else if api >= link::HELPER_API {
                row(Some(true), "root helper", &format!("{} (API {api})", h.display()));
            } else {
                row(Some(false), "root helper", &format!("{} is older than this client (API {api} < {}). Reinstall: \
                    pkexec install -Dm755 {built} {}", h.display(), link::HELPER_API, link::SYSTEM_HELPER));
            }
        }
        Err(e) => row(Some(false), "root helper", &e),
    }
    let policy = Path::new("/usr/share/polkit-1/actions/org.airdrop.helper.policy");
    row(policy.exists().then_some(true), "polkit action",
        if policy.exists() { "installed" } else { "not installed (pkexec will use the generic admin prompt)" });
    let owl = ["/usr/lib/airdrop/owl", "/usr/local/lib/airdrop/owl", "/usr/local/bin/owl", "/usr/bin/owl"].into_iter().find(|p| Path::new(p).exists());
    match owl {
        Some(owl) => {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(owl);
            let safe = meta.as_ref().is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0);
            row(Some(safe), "OWL", &if safe { owl.to_string() } else { format!("{owl} must be owned by root and not group/world writable") });
            let new = std::fs::read(owl).is_ok_and(|b| b.windows(14).any(|w| w == b"auto / auto-ht"));
            if !new {
                row(Some(false), "OWL version", &format!("older than this client (missing speed options). Reinstall: \
                    <source folder>/scripts/build-owl.sh --install, or reinstall the package (found: {owl})"));
            }
        }
        None => row(Some(false), "OWL", "not installed: run scripts/build-owl.sh, then install it to /usr/local/lib/airdrop/owl"),
    }
    let iface = &cfg.wifi_iface;
    match std::fs::read_link(format!("/sys/class/net/{iface}/phy80211")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())) {
        None => row(Some(false), &format!("Wi-Fi interface {iface}"), "not found or not wireless (set with --iface / config)"),
        Some(phy) => {
            let info = run("iw", &["phy", &phy, "info"]);
            let modes = info.split("Supported interface modes:").nth(1).and_then(|m| m.split("Band").next()).unwrap_or_default();
            row(Some(modes.contains("* monitor")), &format!("{iface} ({phy}) monitor mode"), "");
            let active = info.contains("supports active monitor");
            row(active.then_some(true), &format!("{iface} active monitor (ACKs unicast frames)"),
                if active { "" } else { "not supported by this driver: borrow mode does not need it (its access point \
                    acknowledges for us); shared mode may be slow or fail" });
            let dev = run("iw", &["dev", iface, "info"]);
            let chans = run("iw", &["phy", &phy, "channels"]);
            let country = run("iw", &["reg", "get"]).split("country ").nth(1).and_then(|r| r.split(':').next()).unwrap_or("?").to_string();
            let freq = dev.split(" MHz").next().and_then(|s| s.rsplit('(').next()).map(str::to_string);
            match (link::station_channel(iface), freq) {
                (Some(ch), Some(freq)) => {
                    if channel_block(&chans, &freq).is_some_and(|b| b.contains("No IR")) {
                        row(Some(false), "regulatory domain", &format!("{country}: channel {ch} is No-IR, so the kernel silently drops every \
                            AirDrop frame we send. Fix: sudo iw reg set <your country, e.g. DE>"));
                    } else {
                        row(Some(true), "regulatory domain", &format!("{country}: channel {ch} may transmit"));
                    }
                    if ch == 149 {
                        row(Some(true), "Wi-Fi during AirDrop", "connected on channel 149, AirDrop's own channel: Wi-Fi stays connected (shared mode)");
                    } else {
                        row(None, "Wi-Fi during AirDrop", &format!("connected on channel {ch}: Wi-Fi is paused while `airdrop send/receive` runs \
                            (AirDrop needs channel 149) and reconnects afterwards. --mode shared keeps it up but only reaches an iPhone on your channel"));
                    }
                }
                _ => row(None, "Wi-Fi during AirDrop", "not connected to Wi-Fi: AirDrop runs on channel 149 (borrow mode)"),
            }
            match channel_block(&chans, "5745") {
                Some(b) if !b.contains("No IR") && !b.contains("disabled") => row(Some(true), "channel 149", "may transmit"),
                _ => row(Some(false), "channel 149", "not usable for transmitting in the current regulatory domain (every AirDrop frame \
                    would be dropped silently). Fix: sudo iw reg set <your country, e.g. DE>"),
            }
            let driver = std::fs::read_link(format!("/sys/class/net/{iface}/device/driver")).ok()
                .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
            if Path::new(&format!("/sys/kernel/debug/ieee80211/{phy}/mt76")).exists() || driver.starts_with("mt7") {
                row(None, "MediaTek tuning", "power saving is switched off while AirDrop runs (it silently stops monitor reception on mt76 cards)");
            }
            if driver.starts_with("rtw89_") && Path::new("/sys/module/rtw89_core/parameters/awdl_ba").exists() {
                row(Some(true), &format!("{driver} driver"), "AirDrop build loaded (awdl_ba): the iPhone is a client of the \
                    hidden access point, Block Ack in AWDL's format, firmware aggregation when sending (measured 40 MB/s \
                    to an iPhone 17). Back to stock: <source folder>/scripts/rtw89-load.sh revert, or reboot");
            } else if driver.starts_with("rtw89_") {
                row(None, &format!("{driver} driver"), "stock driver: each session registers the iPhone as a client of the \
                    hidden access point (measured with an iPhone 17: send 5.5 MB/s, single frames at HT rates; receive \
                    20.5 MB/s); aggregated sending needs our rtw89 build: <source folder>/scripts/rtw89-build.sh, then \
                    scripts/rtw89-load.sh load");
            } else if driver != "mt7921e" && !driver.is_empty() {
                row(None, &format!("{driver} driver"), "sending uses HT rates (MCS 15, 40 MHz) and steps down by itself when \
                    the iPhone stops acknowledging; single frames, no aggregation. If transfers are slow, \
                    docs/wiki/Faster-on-your-card.md shows how to find out why");
            }
            if let Ok(now) = std::fs::read_to_string("/sys/module/mt7921e/srcversion").map(|s| s.trim().to_string()) {
                let disk = run("modinfo", &["-F", "srcversion", "mt7921e"]).trim().to_string();
                let ht = std::fs::read_to_string("/run/airdrop-mt7921-ht").is_ok_and(|s| s.trim() == now) || link::HT_DRIVER_SRCVERSIONS.contains(&now.as_str());
                let load = "<source folder>/scripts/mt7921-ratefix.sh load";
                if std::path::Path::new("/sys/module/mt7921e/parameters/awdl_ba").exists() {
                    row(Some(true), "mt7921e driver", "fast build with aggregated sending loaded (measured 30 MB/s to an iPhone 17 at 80 MHz)");
                } else if ht {
                    row(None, "mt7921e driver", &format!("fast build loaded, without aggregated sending: about 5 MB/s to an iPhone \
                        instead of 30. Until the next reboot: {load}"));
                } else if !disk.is_empty() && now != disk {
                    row(None, "mt7921e driver", &format!("rate-fixed build loaded, without the fast (HT) rates: sending tops out near 1 MB/s \
                        instead of 5. Until the next reboot: {load}"));
                } else {
                    row(None, "mt7921e driver", &format!("stock module: sending to an iPhone is many times slower, and shared mode on 5 GHz \
                        cannot transmit. Until the next reboot: {load}"));
                }
            }
        }
    }
    let has_bt = std::fs::read_dir("/sys/class/bluetooth").is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy().starts_with("hci")));
    row(Some(has_bt), "Bluetooth controller", if has_bt { "" } else { "needed for the BLE wake-up and for listen" });
    let nm = Path::new("/run/NetworkManager").exists();
    row(nm.then_some(true), "NetworkManager", if nm { "" } else { "not running: borrow mode cannot pause and reconnect Wi-Fi" });
    println!("\nname: {}   downloads: {}   config: {}", clean(&cfg.name), cfg.download_dir.display(), config_path().display());
    i32::from(!ok_all)
}
