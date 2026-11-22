//! The AirDrop receiver: a TLS server answering /Discover, /Ask and /Upload.
//!
//! A transfer from one peer goes through explicit states:
//!
//! ```text
//! /Ask ──200 at once──▶ Deciding ──yes──▶ Accepted ──/Upload──▶ Uploading ──trailer──▶ Saved
//!                          │                                       │
//!                          └──no──▶ Declined (/Upload gets 401)    └──error/stall/stop──▶ Aborted
//! ```
//!
//! * The /Ask is answered before its body has arrived and before anyone decided: iOS
//!   times the round trip, and a late answer reads as "declined" on the iPhone.
//! * Consent still gates every byte written to the download folder: while the question
//!   is open, the upload goes to an unnamed spool (16 MiB in memory, at most 256 MiB on
//!   disk, then TCP backpressure).
//! * The 200 for an upload is sent when the archive's trailer arrives: iOS 26+ holds back
//!   its final empty chunk until it has our reply.
//! * Files are written by a thread that owns the extractor. Dropping an unfinished one,
//!   whatever the reason (error, stall, Ctrl-C), removes everything the transfer wrote.

use crate::config::Config;
use crate::conn::{Conn, ConnError};
use crate::dump::{headers_json, Dump};
use crate::progress::Progress;
use crate::prompt::{describe, notify, Decision, Prompter};
use crate::util::{clean, human, which};
use crate::verbose;
use airdrop_proto::extract::{ExtractError, Extractor, Member};
use airdrop_proto::http::{self, BodyDecoder, Head, MAX_SMALL_BODY};
use airdrop_proto::wire::{self, AskInfo};
use openssl::ssl::{Ssl, SslAcceptor};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio_openssl::SslStream;

/// Waiting for the next request head on an open connection.
const IDLE_TIMEOUT: Duration = Duration::from_secs(180);
/// An accepted /Ask is good for one /Upload within this time.
const GRANT_TTL: Duration = Duration::from_secs(180);
const SPOOL_IN_MEMORY: usize = 16 << 20;
const SPOOL_MAX: u64 = 256 << 20;
const MAX_CONNECTIONS: usize = 64;
const HEAD_LIMIT: usize = 256 * 1024;
const NO_UPLOAD_REPORT: Duration = Duration::from_secs(5);

/// Everything the receiver needs to know, read once at start.
#[derive(Clone)]
pub struct Settings {
    pub media_caps: Option<Vec<u8>>,
    pub contact_exchange: bool,
    /// Experiments E5.1/E5.2: answer /Ask with this status, after this delay.
    pub ask_status: u16,
    pub ask_delay: Duration,
    /// Experiments E5.4/E5.7: the status for a path we do not implement (400 unless set;
    /// 200 comes with an empty plist, to see what the sender does next).
    pub unknown_status: u16,
    pub stall: Duration,
    pub open_urls: bool,
}

/// What `airdrop listen` watches to decide when to give Wi-Fi back.
pub struct Activity {
    last: Mutex<Instant>,
    completed: AtomicU32,
    requests: AtomicU32,
    open: AtomicUsize,
}

impl Activity {
    fn touch(&self) {
        *self.last.lock().expect("not poisoned") = Instant::now();
    }

    pub fn last(&self) -> Instant {
        *self.last.lock().expect("not poisoned")
    }

    pub fn completed(&self) -> u32 {
        self.completed.load(Ordering::Relaxed)
    }

    pub fn requests(&self) -> u32 {
        self.requests.load(Ordering::Relaxed)
    }

    /// Something is happening: a request or a transfer in progress, or a question to the
    /// user. An idle keep-alive connection is not: an iPhone keeps its open after a
    /// transfer, and `listen` must still give Wi-Fi back (2026-11-13, Wi-Fi stayed paused).
    pub fn busy(&self) -> bool {
        self.open.load(Ordering::Relaxed) > 0
    }
}

struct Grant {
    /// Names the question and its transfer in `--json` events.
    id: u64,
    info: AskInfo,
    expires: Instant,
    /// `None` while undecided, then the answer.
    decision: watch::Receiver<Option<bool>>,
    task: AbortHandle,
}

/// A question or an accepted request that ends without an upload (the sender asked again,
/// or we stopped): the shell's card for it must go.
fn abandoned(g: &Grant) {
    g.task.abort();
    crate::events::drop_question(g.id);
    if *g.decision.borrow() != Some(false) {
        crate::events::emit("transfer", serde_json::json!({ "id": g.id, "dir": "in", "phase": "cancelled",
            "message": "The sender stopped before sending" }));
    }
}

struct Inner {
    cfg: Mutex<Config>,
    settings: Settings,
    prompter: Prompter,
    acceptor: SslAcceptor,
    dump: Option<Arc<Dump>>,
    grants: Mutex<HashMap<IpAddr, Grant>>,
    activity: Activity,
}

pub struct Receiver {
    inner: Arc<Inner>,
    shutdown: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Receiver {
    /// Listen on `addr` (port 0 = any); returns the receiver and its port.
    pub async fn start(addr: SocketAddr, cfg: Config, settings: Settings, prompter: Prompter, acceptor: SslAcceptor,
                       dump: Option<Arc<Dump>>) -> std::io::Result<(Self, u16)> {
        let socket = if addr.is_ipv6() { TcpSocket::new_v6()? } else { TcpSocket::new_v4()? };
        socket.set_reuseaddr(true)?;
        socket.bind(addr)?;
        let listener = socket.listen(64)?;
        let port = listener.local_addr()?.port();
        let inner = Arc::new(Inner {
            cfg: Mutex::new(cfg),
            settings,
            prompter,
            acceptor,
            dump,
            grants: Mutex::new(HashMap::new()),
            activity: Activity { last: Mutex::new(Instant::now()), completed: AtomicU32::new(0), requests: AtomicU32::new(0), open: AtomicUsize::new(0) },
        });
        let (tx, rx) = oneshot::channel();
        let server = tokio::spawn(serve(listener, inner.clone(), rx));
        Ok((Self { inner, shutdown: Some(tx), server: Some(server) }, port))
    }

    pub fn activity(&self) -> &Activity {
        &self.inner.activity
    }

    /// Stop accepting, cancel every connection (a cancelled upload removes its partial
    /// files before this returns) and drop open questions.
    pub async fn close(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(server) = self.server.take() {
            let _ = server.await;
        }
        for (_, g) in self.inner.grants.lock().expect("not poisoned").drain() {
            abandoned(&g);
        }
    }
}

async fn serve(listener: TcpListener, inner: Arc<Inner>, mut shutdown: oneshot::Receiver<()>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) if conns.len() < MAX_CONNECTIONS => {
                    let inner = inner.clone();
                    conns.spawn(async move {
                        handle(&inner, tcp, peer).await;
                    });
                }
                Ok(_) => verbose!("too many connections; refused one"),
                Err(e) => {
                    verbose!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
    // A peer that is still sending never answers our TLS close: drop connections outright.
    conns.abort_all();
    let _ = tokio::time::timeout(Duration::from_secs(5), async { while conns.join_next().await.is_some() {} }).await;
}

/// Counts one piece of work in `Activity::busy` while it lives.
struct OpenGuard<'a>(&'a AtomicUsize);

impl<'a> OpenGuard<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::Relaxed);
        Self(n)
    }
}

impl Drop for OpenGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

type Stream = SslStream<TcpStream>;

async fn handle(inner: &Arc<Inner>, tcp: TcpStream, peer: SocketAddr) {
    let ip = peer.ip();
    let fd = tcp.as_raw_fd();
    let Ok(ssl) = Ssl::new(inner.acceptor.context()) else { return };
    let Ok(mut stream) = SslStream::new(ssl, tcp) else { return };
    match tokio::time::timeout(Duration::from_secs(30), Pin::new(&mut stream).accept()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return verbose!("TLS handshake with {ip} failed: {e}"),
        Err(_) => return verbose!("TLS handshake with {ip} timed out"),
    }
    if let Some(d) = &inner.dump {
        d.connection(&ip.to_string(), Some(stream.ssl()));
    }
    let mut conn = Conn::new(stream, HEAD_LIMIT);
    loop {
        let head = match tokio::time::timeout(IDLE_TIMEOUT, conn.read_head()).await {
            Ok(Ok(Some(h))) => h,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                verbose!("connection from {ip} ended: {e}");
                break;
            }
            Err(_) => {
                verbose!("connection from {ip} idle for {} s; closed", IDLE_TIMEOUT.as_secs());
                break;
            }
        };
        let Some((method, path)) = head.request_line().map(|(m, p)| (m.to_string(), p.to_string())) else {
            verbose!("bad request line from {ip}: {:?}", head.start);
            break;
        };
        verbose!("{} {} from {ip}", clean(&method), clean(&path));
        inner.activity.touch();
        inner.activity.requests.fetch_add(1, Ordering::Relaxed);
        if head.headers.get("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) && conn.write_all(http::CONTINUE).await.is_err() {
            break;
        }
        let result = {
            let _busy = OpenGuard::new(&inner.activity.open);
            dispatch(inner, &method, &path, &head, &mut conn, ip, fd).await
        };
        match result {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                verbose!("connection from {ip} ended: {e}");
                break;
            }
        }
    }
    conn.shutdown().await;
}

async fn reply(conn: &mut Conn<Stream>, status: u16, body: &[u8], keep: bool) -> Result<bool, ConnError> {
    conn.write_all(&http::response(status, body, keep)).await?;
    Ok(keep)
}

async fn dispatch(inner: &Arc<Inner>, method: &str, path: &str, head: &Head, conn: &mut Conn<Stream>, ip: IpAddr, fd: i32)
    -> Result<bool, ConnError> {
    let peer = ip.to_string();
    let (name, model) = {
        let cfg = inner.cfg.lock().expect("not poisoned");
        (cfg.name.clone(), cfg.model.clone())
    };
    if method != "POST" {
        conn.read_body_within(head, MAX_SMALL_BODY, IDLE_TIMEOUT).await?;
        return reply(conn, 405, b"", true).await;
    }
    match path {
        "/Discover" => {
            let body = conn.read_body_within(head, MAX_SMALL_BODY, IDLE_TIMEOUT).await?;
            if let Some(d) = &inner.dump {
                d.request(&peer, method, path, headers_json(&head.headers), &body, Some(200), None);
            }
            reply(conn, 200, &wire::discover_response(&name, &model, inner.settings.media_caps.as_deref()), true).await
        }
        "/Ask" => ask(inner, head, conn, ip, fd, &name, &model).await,
        "/Upload" => {
            let grant = inner.grants.lock().expect("not poisoned").remove(&ip);
            let Some(grant) = grant.filter(|g| g.expires > Instant::now()) else {
                eprintln!("Refused an upload from {peer} without an accepted request.");
                return reply(conn, 403, b"", false).await; // body not read: the connection goes
            };
            if *grant.decision.borrow() == Some(false) {
                return reply(conn, 401, b"", false).await;
            }
            upload(inner, grant, head, conn, &peer).await
        }
        _ => {
            let status = inner.settings.unknown_status;
            let body = conn.read_body_within(head, MAX_SMALL_BODY, IDLE_TIMEOUT).await?;
            if let Some(d) = &inner.dump {
                d.request(&peer, method, path, headers_json(&head.headers), &body, Some(status), Some("unknown path"));
            }
            eprintln!("Request for {} from {peer}: not implemented, answered {status}.", clean(path));
            let empty = if status == 200 { wire::dumps(Default::default()) } else { Vec::new() };
            reply(conn, status, &empty, true).await // 400: owly HTTP-5
        }
    }
}

async fn ask(inner: &Arc<Inner>, head: &Head, conn: &mut Conn<Stream>, ip: IpAddr, fd: i32, name: &str, model: &str)
    -> Result<bool, ConnError> {
    let peer = ip.to_string();
    let s = &inner.settings;
    if !s.ask_delay.is_zero() {
        tokio::time::sleep(s.ask_delay).await;
    }
    if s.ask_status != 200 {
        let body = conn.read_body_within(head, MAX_SMALL_BODY, IDLE_TIMEOUT).await?;
        if let Some(d) = &inner.dump {
            let note = format!("experiment: /Ask answered {} after {} s", s.ask_status, s.ask_delay.as_secs_f64());
            d.request(&peer, "POST", "/Ask", headers_json(&head.headers), &body, Some(s.ask_status), Some(&note));
        }
        return reply(conn, s.ask_status, b"", true).await;
    }
    // Early ask: 200 first, consent afterwards (see the module comment).
    let started = Instant::now();
    conn.write_all(&http::response(200, &wire::ask_response(name, model, s.contact_exchange), true)).await?;
    let replied = started.elapsed();
    let body = conn.read_body_within(head, MAX_SMALL_BODY, IDLE_TIMEOUT).await?;
    verbose!("/Ask from {peer}: 200 sent after {:.0} ms, body complete ({} bytes) after {:.0} ms",
        replied.as_secs_f64() * 1e3, body.len(), started.elapsed().as_secs_f64() * 1e3);
    if let Some(d) = &inner.dump {
        d.request(&peer, "POST", "/Ask", headers_json(&head.headers), &body, Some(200), Some("early ask: 200 sent before the body was read"));
    }
    let Ok(info) = AskInfo::parse(&body) else { return Ok(false) };
    let id = crate::events::next_id();
    let (tx, rx) = watch::channel(None);
    let task = {
        let (inner, info) = (inner.clone(), info.clone());
        tokio::spawn(async move {
            let accepted = decide(&inner, &info, id).await;
            let _ = tx.send(Some(accepted));
        })
    };
    let grant = Grant { id, info, expires: Instant::now() + GRANT_TTL, decision: rx.clone(), task: task.abort_handle() };
    if let Some(old) = inner.grants.lock().expect("not poisoned").insert(ip, grant) {
        abandoned(&old);
    }
    if crate::is_verbose() {
        let inner = inner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(NO_UPLOAD_REPORT).await;
            report_no_upload(&inner, ip, &rx, fd);
        });
    }
    Ok(true)
}

/// -v only: the sender asked, we said yes (or still ask), and it has not started the upload
/// (on the iPhone: "Waiting..."). Say whether its TCP took our reply.
fn report_no_upload(inner: &Inner, ip: IpAddr, rx: &watch::Receiver<Option<bool>>, fd: i32) {
    // Read the answer, then let go of the lock before the syscall and the print.
    let answer = {
        let grants = inner.grants.lock().expect("not poisoned");
        let Some(g) = grants.get(&ip).filter(|g| g.decision.same_channel(rx)) else { return };
        match *g.decision.borrow() {
            Some(false) => return, // declined: no upload is expected
            Some(true) => "accepted",
            None => "the question is still open",
        }
    };
    eprintln!("DEBUG airdrop: no /Upload from {ip} {} s after its /Ask ({answer}): {}", NO_UPLOAD_REPORT.as_secs(), tcp_state(fd));
}

/// Our side of a connection (Linux TCP_INFO): did the peer's TCP take what we sent?
fn tcp_state(fd: i32) -> String {
    let mut info = [0u32; 26];
    let mut len = std::mem::size_of_val(&info) as libc::socklen_t;
    // SAFETY: getsockopt writes at most len bytes into info.
    let rc = unsafe { libc::getsockopt(fd, libc::IPPROTO_TCP, libc::TCP_INFO, info.as_mut_ptr().cast(), &mut len) };
    if rc != 0 || len < 104 {
        return "connection already closed".into();
    }
    // 8 one-byte fields (2 u32s), then u32s: unacked, last_data_recv, last_ack_recv, total_retrans.
    let f = &info[2..];
    format!("{} of our segments not acknowledged, {} retransmitted, last ACK from the peer {:.1} s ago, last data from it {:.1} s ago",
        f[4], f[23], f64::from(f[12]) / 1000.0, f64::from(f[11]) / 1000.0)
}

/// The `ask` event: who sends what, and whether someone has to answer.
fn ask_event(info: &AskInfo, id: u64, known: bool, asking: bool, timeout: u64) {
    if !crate::events::on() {
        return;
    }
    let files: Vec<serde_json::Value> = info.files.iter().map(|f| serde_json::json!({
        "name": clean(&f.name), "uti": clean(&f.uti), "size": f.size, "dir": f.is_dir,
    })).collect();
    let total: Option<u64> = info.files.iter().map(|f| f.size).sum();
    // The sender's preview, for the shell to show if it can decode it (an iPhone sends JPEG 2000).
    let icon = info.icon.as_ref().and_then(|bytes| {
        use std::os::unix::fs::OpenOptionsExt;
        let ext = if bytes.starts_with(&[0xff, 0xd8]) { "jpg" } else if bytes.starts_with(b"\x89PNG") { "png" } else { "jp2" };
        let path = crate::events::runtime_dir()?.join(format!("icon-{id}.{ext}"));
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path).ok()?;
        f.write_all(bytes).ok()?;
        Some(path.display().to_string())
    });
    crate::events::emit("ask", serde_json::json!({
        "id": id, "sender": info.sender_name.as_deref().map(clean), "model": clean(&info.sender_model),
        "kind": crate::events::kind(&info.sender_model), "known": known, "asking": asking, "timeout": timeout,
        "files": files, "urls": info.urls.iter().map(|u| clean(u)).collect::<Vec<_>>(), "total": total, "icon": icon,
    }));
}

async fn decide(inner: &Inner, info: &AskInfo, id: u64) -> bool {
    let name = clean(info.sender_label());
    let known = info.sender_name.as_deref().is_some_and(|n| inner.cfg.lock().expect("not poisoned").is_known(n));
    let (decision, by) = if known {
        eprintln!("AirDrop from known sender {name}: {} (auto-accepted)", describe(info));
        ask_event(info, id, true, false, 0);
        (Decision::Accept, "known")
    } else {
        let _busy = OpenGuard::new(&inner.activity.open);
        let p = &inner.prompter;
        p.ask(info, id, || ask_event(info, id, false, p.is_interactive(), p.timeout())).await
    };
    crate::events::emit("answered", serde_json::json!({ "id": id, "by": by, "decision": match decision {
        Decision::Accept => "accept",
        Decision::Always => "always",
        Decision::Decline => "decline",
    } }));
    if decision == Decision::Always {
        match &info.sender_name {
            Some(sender) => {
                if let Err(e) = inner.cfg.lock().expect("not poisoned").add_known(sender) {
                    eprintln!("Could not save the known senders: {e}");
                }
                eprintln!("{name} will be accepted automatically from now on.");
            }
            None => eprintln!("Accepted once: the sender gave no name, so it cannot be remembered."),
        }
    }
    if decision == Decision::Decline {
        eprintln!("Declined AirDrop from {name}.");
        return false;
    }
    // Links open only when a person said yes here: names are self-reported, and anyone
    // nearby can send when everything is accepted.
    let open = inner.settings.open_urls && !known && inner.prompter.is_interactive();
    for url in &info.urls {
        crate::say!("LINK {}", clean(url));
        crate::events::emit("link", serde_json::json!({ "id": id, "url": clean(url), "sender": info.sender_name.as_deref().map(clean) }));
        let web = url.starts_with("https://") || url.starts_with("http://");
        if open && web {
            if let Some(exe) = which("xdg-open") {
                use std::os::unix::process::CommandExt;
                let _ = std::process::Command::new(exe).arg(url)
                    .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                    .process_group(0).spawn();
            }
        } else if open {
            eprintln!("Not opening {}: only http and https links are opened.", clean(url));
        }
    }
    true
}

// ---- the extractor thread ----------------------------------------------------------

struct Outcome {
    saved: Vec<PathBuf>,
    members: Vec<Member>,
    bytes_written: u64,
}

enum Msg {
    Data(Vec<u8>),
    Finish(oneshot::Sender<Result<Outcome, ExtractError>>),
}

/// The extractor on its own thread, behind a bounded queue (disk slower than the link =
/// TCP backpressure). Dropped without `finish`, it removes what it wrote before returning.
struct Sink {
    tx: Option<mpsc::Sender<Msg>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The archive's trailer arrived.
    done: watch::Receiver<bool>,
}

impl Sink {
    fn start(mut ex: Extractor) -> Self {
        let (tx, mut rx) = mpsc::channel::<Msg>(16);
        let (done_tx, done) = watch::channel(false);
        let thread = std::thread::spawn(move || {
            let mut failed: Option<ExtractError> = None;
            while let Some(msg) = rx.blocking_recv() {
                match msg {
                    Msg::Data(d) => {
                        if failed.is_none() {
                            if let Err(e) = ex.feed(&d) {
                                failed = Some(e);
                                let _ = done_tx.send(true); // wake the reader: it finishes and learns why
                            } else if ex.is_done() {
                                let _ = done_tx.send(true);
                            }
                        }
                    }
                    Msg::Finish(reply) => {
                        let result = match failed.take() {
                            Some(e) => Err(e),
                            None => ex.finish().map(|saved| Outcome { saved, members: ex.members().to_vec(), bytes_written: ex.bytes_written() }),
                        };
                        let _ = reply.send(result);
                        return; // `ex` drops here: it removes everything unless finish succeeded
                    }
                }
            }
        });
        Self { tx: Some(tx), thread: Some(thread), done }
    }

    /// False when the thread is gone (it failed; `finish` says why).
    async fn put(&self, data: Vec<u8>) -> bool {
        match &self.tx {
            Some(tx) => tx.send(Msg::Data(data)).await.is_ok(),
            None => false,
        }
    }

    async fn finish(mut self) -> Result<Outcome, ExtractError> {
        let (tx, rx) = oneshot::channel();
        let sent = match &self.tx {
            Some(sender) => sender.send(Msg::Finish(tx)).await.is_ok(),
            None => false,
        };
        let result = if sent { rx.await.ok() } else { None };
        self.close();
        result.unwrap_or_else(|| Err(ExtractError::Io(std::io::Error::other("the extractor stopped"))))
    }

    fn close(&mut self) {
        self.tx = None; // the thread's queue ends: it drops the extractor (removing files) and exits
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.close();
    }
}

// ---- spool ---------------------------------------------------------------------------

/// Upload bytes kept while the user decides: memory first, then an unnamed file in the
/// download folder (never visible, gone when closed).
struct Spool {
    mem: Vec<u8>,
    file: Option<File>,
    dir: PathBuf,
    len: u64,
}

impl Spool {
    fn new(dir: PathBuf) -> Self {
        Self { mem: vec![], file: None, dir, len: 0 }
    }

    fn full(&self) -> bool {
        self.len >= SPOOL_MAX
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.len += data.len() as u64;
        if self.file.is_none() && self.mem.len() + data.len() <= SPOOL_IN_MEMORY {
            self.mem.extend_from_slice(data);
            return Ok(());
        }
        if self.file.is_none() {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().read(true).write(true).mode(0o600).custom_flags(libc::O_TMPFILE).open(&self.dir)?;
            f.write_all(&std::mem::take(&mut self.mem))?;
            self.file = Some(f);
        }
        self.file.as_mut().expect("just set").write_all(data)
    }

    /// The spooled bytes, in order, in blocks.
    fn drain(self) -> std::io::Result<Vec<Vec<u8>>> {
        let Some(mut f) = self.file else { return Ok(if self.mem.is_empty() { vec![] } else { vec![self.mem] }) };
        f.rewind()?;
        let mut blocks = vec![];
        loop {
            let mut b = vec![0u8; 1 << 20];
            let n = f.read(&mut b)?;
            if n == 0 {
                return Ok(blocks);
            }
            b.truncate(n);
            blocks.push(b);
        }
    }
}

// ---- /Upload -------------------------------------------------------------------------

async fn upload(inner: &Arc<Inner>, mut grant: Grant, head: &Head, conn: &mut Conn<Stream>, peer: &str) -> Result<bool, ConnError> {
    let dest = inner.cfg.lock().expect("not poisoned").download_dir.clone();
    let name = clean(grant.info.sender_label());
    if let Err(e) = std::fs::create_dir_all(&dest) {
        eprintln!("Could not save files: {}: {e}", dest.display());
        return reply(conn, 500, b"", false).await;
    }
    let headers = &head.headers;
    // owly UP-1/UP-4: DVZip (iPhones) or gzip cpio (Macs); anything else is refused unread.
    let kind = headers.get("content-type").map(|c| c.split(';').next().unwrap_or("").trim().to_ascii_lowercase());
    let dvzip = match kind.as_deref() {
        Some("application/x-dvzip") => true,
        Some("application/x-cpio") => false,
        other => {
            eprintln!("Refused the upload from {name}: unsupported Content-Type {:?}.", other.map(clean).unwrap_or_default());
            return reply(conn, 406, b"", false).await;
        }
    };
    let roots: Vec<String> = grant.info.files.iter().map(|f| f.bom_path.clone()).collect();
    let names: Vec<String> = grant.info.files.iter().map(|f| f.name.clone()).collect();
    let sink = Sink::start(Extractor::new(&dest, dvzip, &roots, &names));
    let total = headers.get("totalbytes").and_then(|t| t.parse().ok());
    let progress = Progress::new(total, format!("Receiving from {name}"), Some(grant.id));
    let id = grant.id;
    let failed = |message: String| crate::events::emit("transfer", serde_json::json!({
        "id": id, "dir": "in", "phase": "failed", "message": message,
    }));
    let mut dec = BodyDecoder::new(headers)?;

    if grant.decision.borrow().is_none() && !spool_until_decided(&mut grant, conn, &mut dec, &sink, &progress, &inner.activity, &dest).await? {
        progress.finish();
        reply(conn, 401, b"", false).await?; // declined: nothing was written
        conn.linger(Duration::from_secs(2)).await; // so the sender reads the 401, not a reset
        return Ok(false);
    }
    crate::events::emit("transfer", serde_json::json!({ "id": id, "dir": "in", "phase": "receiving", "total": total,
        "sender": grant.info.sender_name.as_deref().map(clean) }));
    let (outcome, body_done) = match stream_to_sink(sink, conn, &mut dec, &progress, &inner.activity, inner.settings.stall).await {
        Ok(Streamed::Finished(outcome, body_done)) => (outcome, body_done),
        Ok(Streamed::Stalled) => {
            progress.finish();
            eprintln!("Transfer from {name} stalled after {} (no data for {:.0} s); nothing was saved. Send it again from the device.",
                human(progress.done() as f64), inner.settings.stall.as_secs_f64());
            failed(format!("{name} stopped sending; nothing was saved"));
            return Ok(false);
        }
        Ok(Streamed::CutOff) => {
            progress.finish();
            eprintln!("Transfer from {name} was cut off after {}; nothing was saved.", human(progress.done() as f64));
            failed(format!("{name} stopped sending; nothing was saved"));
            return Err(ConnError::Closed);
        }
        Err(e) => {
            failed(format!("The transfer from {name} broke off; nothing was saved"));
            return Err(e);
        }
    };
    let elapsed = progress.finish();
    let outcome = match outcome {
        Ok(o) => o,
        Err(ExtractError::Unsafe(why)) => {
            eprintln!("Rejected upload from {name}: {}", clean(&why));
            failed(format!("Refused: {}", clean(&why)));
            return reply(conn, 400, b"", false).await;
        }
        Err(ExtractError::Io(e)) => {
            eprintln!("Could not save files: {e}");
            failed(format!("Could not save the files: {e}"));
            return reply(conn, 500, b"", false).await;
        }
    };
    if let Some(d) = &inner.dump {
        d.upload(peer, headers, if dvzip { "dvzip" } else { "auto" }, &outcome.members, 200, progress.done());
    }
    conn.write_all(&http::response(200, b"", body_done)).await?;
    eprintln!("Received {} from {name} in {elapsed:.1}s", human(outcome.bytes_written as f64));
    for p in &outcome.saved {
        crate::say!("SAVED {}", clean(&p.to_string_lossy()));
    }
    crate::events::emit("transfer", serde_json::json!({
        "id": id, "dir": "in", "phase": "done", "sender": grant.info.sender_name.as_deref().map(clean),
        "saved": outcome.saved.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "bytes": outcome.bytes_written, "seconds": (elapsed * 10.0).round() / 10.0,
    }));
    inner.activity.completed.fetch_add(1, Ordering::Relaxed);
    inner.activity.touch();
    let n = outcome.saved.len();
    notify(&format!("AirDrop from {name}"), &format!("Saved {n} item{} to {}", if n == 1 { "" } else { "s" }, dest.display()));
    if !body_done {
        // Let the sender finish the body it held back, so it reads our 200 rather than a
        // reset; then close.
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while let Ok(Some(_)) = conn.body_chunk(&mut dec).await {}
        }).await;
        return Ok(false);
    }
    Ok(true)
}

/// Deciding: keep what arrives in a spool, with no progress line (it would overwrite the
/// question on screen). Returns the answer; once accepted, the spool is in the sink.
async fn spool_until_decided(grant: &mut Grant, conn: &mut Conn<Stream>, dec: &mut BodyDecoder, sink: &Sink, progress: &Progress,
                             activity: &Activity, dest: &Path) -> Result<bool, ConnError> {
    let shown = progress.enabled();
    progress.set_enabled(false);
    let mut spool = Spool::new(dest.to_path_buf());
    let accepted = loop {
        while let Some(chunk) = conn.take_buffered(dec)? {
            spool.write(&chunk).map_err(ConnError::Io)?;
            progress.add(chunk.len() as u64);
            activity.touch();
        }
        if let Some(d) = *grant.decision.borrow() {
            break d;
        }
        tokio::select! {
            more = conn.fill(), if !dec.is_done() && !spool.full() => if !more? { return Err(ConnError::Closed) },
            changed = grant.decision.changed() => if changed.is_err() { break false }, // replaced by a newer /Ask
        }
    };
    if !accepted {
        return Ok(false);
    }
    progress.set_enabled(shown);
    for block in spool.drain().map_err(ConnError::Io)? {
        if !sink.put(block).await {
            break;
        }
    }
    Ok(true)
}

enum Streamed {
    /// The extractor's result, and whether the HTTP body ended too.
    Finished(Result<Outcome, ExtractError>, bool),
    /// No data for the stall time; the sink is gone with what it wrote.
    Stalled,
    /// The sender closed the connection mid-body (the dropped sink removed its files).
    CutOff,
}

/// Accepted: stream into the extractor until the body ends or the archive is complete.
async fn stream_to_sink(sink: Sink, conn: &mut Conn<Stream>, dec: &mut BodyDecoder, progress: &Progress, activity: &Activity,
                        stall: Duration) -> Result<Streamed, ConnError> {
    let mut trailer = sink.done.clone();
    let mut last_rx = Instant::now();
    loop {
        let mut failed = false;
        while let Some(chunk) = conn.take_buffered(dec)? {
            last_rx = Instant::now();
            activity.touch();
            progress.add(chunk.len() as u64);
            if !sink.put(chunk).await {
                failed = true;
                break;
            }
        }
        if failed || dec.is_done() || *trailer.borrow() {
            let body_done = dec.is_done();
            return Ok(Streamed::Finished(sink.finish().await, body_done));
        }
        tokio::select! {
            more = conn.fill() => if !more? { return Ok(Streamed::CutOff) },
            _ = trailer.changed() => {}
            () = tokio::time::sleep_until((last_rx + stall).into()) => {
                drop(sink); // removes what was written, before anything else
                return Ok(Streamed::Stalled);
            }
        }
    }
}
