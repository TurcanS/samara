//! Deciding whether to accept an incoming AirDrop: always the same answer (the default),
//! a question in this terminal, an actionable desktop notification, or (`--json`) an `ask`
//! event answered on stdin.

use crate::util::clean;
use airdrop_proto::wire::AskInfo;
use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Decline,
    /// Accept, and from now on this sender without asking.
    Always,
}

pub fn describe(info: &AskInfo) -> String {
    if !info.urls.is_empty() {
        return format!("link: {}", clean(&info.urls.join(", ")));
    }
    if info.files.is_empty() {
        return "nothing".into();
    }
    let names: Vec<String> = info.files.iter().map(|f| clean(&f.name) + if f.is_dir { "/" } else { "" }).collect();
    let mut shown = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 5 {
        shown.push_str(&format!(" and {} more", names.len() - 5));
    }
    format!("{} item{}: {shown}", names.len(), if names.len() == 1 { "" } else { "s" })
}

/// Best-effort desktop notification; never blocks or fails anything.
pub fn notify(summary: &str, body: &str) {
    // With --json the program driving us shows its own; a notification would be a duplicate.
    if crate::events::on() || !desktop::available() {
        return;
    }
    let (summary, body) = (summary.to_string(), body.to_string());
    std::thread::spawn(move || {
        let _ = desktop::show(&summary, &body, &[], -1);
    });
}

/// Lines typed on stdin, read by one thread for the whole process (`None` = closed).
/// A plain thread, not the runtime's: a blocked terminal read must not hold up exit.
pub fn stdin_lines() -> &'static Mutex<mpsc::Receiver<Option<String>>> {
    static LINES: OnceLock<Mutex<mpsc::Receiver<Option<String>>>> = OnceLock::new();
    LINES.get_or_init(|| {
        let (tx, rx) = mpsc::channel(16);
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.blocking_send(None);
                        return;
                    }
                    Ok(_) => {
                        if tx.blocking_send(Some(line.trim_end_matches(['\n', '\r']).to_string())).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Mutex::new(rx)
    })
}

#[derive(Clone)]
pub enum Prompter {
    Static(Decision),
    Terminal { timeout: Duration },
    Notification { timeout: Duration },
    /// `--json`: an `ask` event, answered by `{"answer": ..., "id": N}` on stdin.
    Stdin { timeout: Duration },
}

impl Prompter {
    /// Whether a person answers (links are opened only after a person said yes).
    pub fn is_interactive(&self) -> bool {
        !matches!(self, Self::Static(_))
    }

    /// Seconds a person has to answer (0 = nobody is asked).
    pub fn timeout(&self) -> u64 {
        match self {
            Self::Static(_) => 0,
            Self::Terminal { timeout } | Self::Notification { timeout } | Self::Stdin { timeout } => timeout.as_secs(),
        }
    }

    /// The decision, and who made it: `user`, or `timeout` when nobody answered in time.
    /// `announce` runs once the answer can be taken (it emits the `ask` event).
    pub async fn ask(&self, info: &AskInfo, id: u64, announce: impl FnOnce()) -> (Decision, &'static str) {
        match self {
            Self::Static(d) => {
                announce();
                (*d, "auto")
            }
            Self::Terminal { timeout } => {
                announce();
                terminal(info, *timeout).await
            }
            Self::Notification { timeout } => {
                announce();
                notification(info, *timeout).await
            }
            Self::Stdin { timeout } => {
                let answer = crate::events::expect_answer(id);
                announce();
                match tokio::time::timeout(*timeout, answer).await {
                    Ok(Ok(d)) => (d, "user"),
                    Ok(Err(_)) => (Decision::Decline, "user"), // stdin closed: we are stopping
                    Err(_) => {
                        crate::events::drop_question(id);
                        (Decision::Decline, "timeout")
                    }
                }
            }
        }
    }
}

async fn terminal(info: &AskInfo, timeout: Duration) -> (Decision, &'static str) {
    // One question at a time: the lock on the line source is held until it is answered.
    let mut lines = stdin_lines().lock().await;
    let name = clean(info.sender_label());
    notify(&format!("AirDrop from {name}"), &describe(info));
    let model = if info.sender_model.is_empty() { String::new() } else { format!(" ({})", clean(&info.sender_model)) };
    eprintln!("\nAirDrop from {name}{model}: {}", describe(info));
    loop {
        eprint!("Accept? [y]es / [n]o / [a]lways from {name} ({}s): ", timeout.as_secs());
        let _ = std::io::stderr().flush();
        match tokio::time::timeout(timeout, lines.recv()).await {
            Err(_) => {
                eprintln!("\nNo answer, declined.");
                return (Decision::Decline, "timeout");
            }
            Ok(None | Some(None)) => return (Decision::Decline, "user"), // stdin closed
            Ok(Some(Some(line))) => match line.trim().to_lowercase().as_str() {
                "y" | "yes" => return (Decision::Accept, "user"),
                "n" | "no" => return (Decision::Decline, "user"),
                "a" | "always" => return (Decision::Always, "user"),
                _ => {} // Enter alone (or anything else) asks again
            },
        }
    }
}

async fn notification(info: &AskInfo, timeout: Duration) -> (Decision, &'static str) {
    if !desktop::available() {
        return (Decision::Decline, "timeout");
    }
    let summary = format!("AirDrop from {}", clean(info.sender_label()));
    let body = describe(info);
    // The outer limit holds even for a server that never reports the notification closed.
    let asking = tokio::task::spawn_blocking(move || desktop::ask(&summary, &body, timeout));
    let answer = tokio::time::timeout(timeout + Duration::from_secs(5), asking).await.ok().and_then(Result::ok).flatten();
    match answer.as_deref() {
        Some("accept") => (Decision::Accept, "user"),
        Some("always") => (Decision::Always, "user"),
        Some(_) => (Decision::Decline, "user"),
        None => (Decision::Decline, "timeout"),
    }
}

/// `org.freedesktop.Notifications` on the session bus (what `notify-send` used to call).
mod desktop {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use zbus::blocking::{Connection, MessageIterator};
    use zbus::message::Type;
    use zbus::zvariant::Value;
    use zbus::MatchRule;

    const DEST: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    const ACTIONS: [&str; 6] = ["accept", "Accept", "always", "Always accept", "decline", "Decline"];

    /// A graphical session with a session bus; without one there is nobody to show it to.
    pub fn available() -> bool {
        (std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()) && bus().is_some()
    }

    fn bus() -> Option<&'static Connection> {
        static BUS: OnceLock<Option<Connection>> = OnceLock::new();
        BUS.get_or_init(|| Connection::session().ok()).as_ref()
    }

    /// Show a notification; returns its id.
    pub fn show(summary: &str, body: &str, actions: &[&str], expire_ms: i32) -> Option<u32> {
        let hints: HashMap<&str, Value> = HashMap::new();
        let reply = bus()?.call_method(Some(DEST), PATH, Some(DEST), "Notify",
            &("AirDrop", 0u32, "folder-download", summary, body, actions, hints, expire_ms)).ok()?;
        reply.body().deserialize::<u32>().ok()
    }

    /// Show a question with Accept / Always accept / Decline; the chosen action key, or None
    /// when it was dismissed, timed out or no notification server answers.
    pub fn ask(summary: &str, body: &str, timeout: Duration) -> Option<String> {
        let bus = bus()?;
        // Listen before showing it, so a quick click cannot come before we listen.
        let rule = MatchRule::builder().msg_type(Type::Signal).interface(DEST).ok()?.path(PATH).ok()?.build();
        let signals = MessageIterator::for_match_rule(rule, bus, Some(16)).ok()?;
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let id = show(summary, body, &ACTIONS, ms)?;
        // Some servers ignore the expiry: close it ourselves, which also ends the wait below.
        let closer = std::thread::spawn(move || {
            let until = Instant::now() + timeout;
            while Instant::now() < until {
                std::thread::park_timeout(until - Instant::now());
            }
            let _ = bus.call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(id,));
        });
        let mut answer = None;
        for msg in signals.flatten() {
            let member = msg.header().member().map(|m| m.to_string());
            match member.as_deref() {
                Some("ActionInvoked") => match msg.body().deserialize::<(u32, String)>() {
                    Ok((got, key)) if got == id => {
                        answer = Some(key);
                        break;
                    }
                    _ => {}
                },
                Some("NotificationClosed") if msg.body().deserialize::<(u32, u32)>().is_ok_and(|(got, _)| got == id) => break,
                _ => {}
            }
        }
        if answer.is_some() {
            // Answered: take the question off the screen now.
            let _ = bus.call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(id,));
        }
        drop(closer); // detached: it only closes an already closed notification
        answer
    }
}
