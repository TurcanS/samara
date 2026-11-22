//! `--json`: what happens as one JSON object per line on stdout, and what to do as JSON
//! lines on stdin, for a desktop shell that draws its own interface around the CLI.
//!
//! Events carry `"event"` and the fields listed in README "Machine interface". Commands:
//! `{"answer": "accept"|"decline"|"always", "id": N}`, `{"pick": PEER, "paths": [...]}` or
//! `{"pick": PEER, "url": "..."}`, `{"cancel": ID}`, `{"done": true}`. `done`, or stdin
//! closing, stops the command the way Ctrl-C does: whoever drives us is gone, and Wi-Fi
//! must not stay paused for nobody.

use crate::prompt::{stdin_lines, Decision};
use crate::stop::Stop;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tokio::sync::{mpsc, oneshot};

static ON: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// A number naming one question or transfer for the rest of the process.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Write one event (nothing without `--json`). One locked write per line, so lines from
/// different tasks never interleave.
pub fn emit(event: &str, fields: Value) {
    if !on() {
        return;
    }
    let mut v = match fields {
        Value::Object(m) => Value::Object(m),
        _ => json!({}),
    };
    v["event"] = json!(event);
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// A failure the caller can act on: `code` says which (README "Machine interface").
pub fn error(code: &str, message: &str) {
    emit("error", json!({ "code": code, "message": message }));
}

/// The code for a message from bringing the link up.
pub fn link_code(message: &str) -> &'static str {
    if message.contains("authorization") || message.contains("pkexec") {
        "auth"
    } else if message.contains("airdrop-helper not found") || message.contains("older than this client") {
        "helper"
    } else if message.contains("airdrop-helper.lock") || message.contains("already running") {
        "busy"
    } else {
        "link"
    }
}

/// What a device name says it is, for an icon.
pub fn kind(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    if m.starts_with("iphone") {
        "iphone"
    } else if m.starts_with("ipad") {
        "ipad"
    } else if m.contains("mac") {
        "mac"
    } else {
        "other"
    }
}

/// What to send, from a `pick`.
pub enum Payload {
    Files(Vec<PathBuf>),
    Link(String),
}

pub enum Command {
    Pick { peer: String, payload: Payload },
    Cancel(u64),
    Done,
}

struct Commands {
    /// Picks, cancels and `done`, for the send loop.
    rx: tokio::sync::Mutex<mpsc::Receiver<Command>>,
    /// Open questions, by id.
    answers: Mutex<HashMap<u64, oneshot::Sender<Decision>>>,
}

static COMMANDS: OnceLock<Commands> = OnceLock::new();

/// Read stdin for the rest of the process (only with `--json`). `done` or EOF asks `stop`.
pub fn start(stop: &Stop) {
    let (tx, rx) = mpsc::channel(16);
    if COMMANDS.set(Commands { rx: tokio::sync::Mutex::new(rx), answers: Mutex::new(HashMap::new()) }).is_err() {
        return;
    }
    let stop = stop.clone();
    tokio::spawn(async move {
        let mut lines = stdin_lines().lock().await;
        loop {
            let Some(Some(line)) = lines.recv().await else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                error("input", &format!("not a JSON command: {}", crate::util::clean(&line)));
                continue;
            };
            if v["done"].as_bool() == Some(true) {
                break;
            }
            if let (Some(answer), Some(id)) = (v["answer"].as_str(), v["id"].as_u64()) {
                let decision = match answer {
                    "accept" => Decision::Accept,
                    "always" => Decision::Always,
                    _ => Decision::Decline,
                };
                let waiter = COMMANDS.get().and_then(|c| c.answers.lock().expect("not poisoned").remove(&id));
                if let Some(w) = waiter {
                    let _ = w.send(decision);
                }
                continue;
            }
            if let Some(id) = v["cancel"].as_u64() {
                let _ = tx.send(Command::Cancel(id)).await;
                continue;
            }
            if let Some(peer) = v["pick"].as_str() {
                let payload = if let Some(url) = v["url"].as_str().filter(|u| !u.trim().is_empty()) {
                    Payload::Link(url.trim().to_string())
                } else {
                    let paths: Vec<PathBuf> = v["paths"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
                        .unwrap_or_default();
                    Payload::Files(paths)
                };
                let _ = tx.send(Command::Pick { peer: peer.to_string(), payload }).await;
            }
        }
        // `done` or stdin closed: whoever drove us is gone.
        let _ = tx.send(Command::Done).await;
        stop.request();
    });
}

/// The next pick, cancel or `done` (`None` without `--json`).
pub async fn next_command() -> Option<Command> {
    match COMMANDS.get() {
        Some(c) => c.rx.lock().await.recv().await,
        None => std::future::pending().await,
    }
}

/// Wait up to `timeout` for the answer to question `id`; `None` = no answer in time.
/// The waiter is registered before the caller emits the question, so a quick answer
/// cannot come before anyone listens.
pub fn expect_answer(id: u64) -> oneshot::Receiver<Decision> {
    let (tx, rx) = oneshot::channel();
    if let Some(c) = COMMANDS.get() {
        c.answers.lock().expect("not poisoned").insert(id, tx);
    }
    rx
}

/// Forget an open question (timed out or the sender went away).
pub fn drop_question(id: u64) {
    if let Some(c) = COMMANDS.get() {
        c.answers.lock().expect("not poisoned").remove(&id);
    }
}

/// Files this process wrote for the shell to show (`FileIcon` previews), removed at exit.
pub fn runtime_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    let dir = PathBuf::from(base).join("airdrop").join(std::process::id().to_string());
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn cleanup() {
    if let Some(base) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        let _ = std::fs::remove_dir_all(PathBuf::from(base).join("airdrop").join(std::process::id().to_string()));
    }
}
