//! The AirDrop sender, as an iPhone does it (measured, owly HTTP-1): /Discover on a
//! connection of its own, then a new connection carrying /Ask and /Upload.

use crate::conn::{Conn, ConnError};
use crate::dump::Dump;
use crate::progress::Progress;
use crate::util::{clean, hex, human, random_bytes, uuid4_upper};
use airdrop_proto::cpio::{plan_archive, ArchiveReader, DvzipBlocks};
use airdrop_proto::http::{self, MAX_SMALL_BODY};
use airdrop_proto::wire::{self, AskItem, AskPayload, DiscoverReply};
use base64::Engine;
use openssl::ssl::{Ssl, SslConnector};
use std::io::Read;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_openssl::SslStream;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// An iPhone leaves our channel for 10-14 s at a time (measured).
const CONNECT_ATTEMPTS: u32 = 3;
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(8);
/// A /Discover answer this recent is not asked for again before /Ask.
const DISCOVER_FRESH: Duration = Duration::from_secs(30);
/// The person on the iPhone has to tap Accept.
const ASK_TIMEOUT: Duration = Duration::from_secs(180);
const UPLOAD_DONE_TIMEOUT: Duration = Duration::from_secs(120);
/// A decompressed /Discover or /Ask reply larger than this is refused.
const MAX_REPLY: u64 = 1 << 20;

#[derive(Debug)]
pub enum SendError {
    /// The receiver said no (or dropped the /Ask): exit code 3.
    Declined(String),
    Failed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined(m) | Self::Failed(m) => f.write_str(m),
        }
    }
}

type Result<T> = std::result::Result<T, SendError>;

#[derive(Debug, Clone)]
pub struct Peer {
    pub addr: SocketAddr,
    /// The interface of a link-local address (for display: `fe80::1%awdl0`).
    pub iface: Option<String>,
    pub service: String,
    /// From /Discover; none = not accepting from everyone.
    pub name: Option<String>,
    pub model: Option<String>,
    pub discovered_at: Option<Instant>,
}

impl Peer {
    pub fn new(addr: SocketAddr, iface: Option<String>) -> Self {
        Self { addr, iface, service: String::new(), name: None, model: None, discovered_at: None }
    }

    pub fn available(&self) -> bool {
        self.name.is_some()
    }

    /// The address as text: `fe80::1%awdl0`.
    pub fn host(&self) -> String {
        match (&self.iface, self.addr.ip()) {
            (Some(i), IpAddr::V6(v6)) => format!("{v6}%{i}"),
            (_, ip) => ip.to_string(),
        }
    }

    pub fn label(&self) -> String {
        self.name.as_deref().map(clean).unwrap_or_else(|| format!("[{}]:{}", self.host(), self.addr.port()))
    }
}

/// `fe80::1%awdl0` with the interface's index as the scope.
pub fn scoped(ip: std::net::Ipv6Addr, port: u16, iface: &str) -> SocketAddr {
    let name = std::ffi::CString::new(iface).unwrap_or_default();
    // SAFETY: if_nametoindex reads a NUL-terminated string.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    SocketAddr::V6(SocketAddrV6::new(ip, port, 0, index))
}

pub struct Sender {
    pub tls: SslConnector,
    pub name: String,
    pub model: String,
    pub id: String,
    pub stall: Duration,
    pub dump: Option<Arc<Dump>>,
}

type Stream = SslStream<TcpStream>;

impl Sender {
    async fn connect(&self, peer: &Peer) -> Result<Conn<Stream>> {
        let fail = |why: String| SendError::Failed(format!("cannot connect to {}: {why}", peer.label()));
        let tcp = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(peer.addr)).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => return Err(fail(e.to_string())),
            Err(_) => return Err(fail("no answer".into())),
        };
        let _ = tcp.set_nodelay(true);
        let mut config = self.tls.configure().map_err(|e| fail(e.to_string()))?;
        config.set_use_server_name_indication(false);
        config.set_verify_hostname(false);
        let ssl: Ssl = config.into_ssl("").map_err(|e| fail(e.to_string()))?;
        let mut stream = SslStream::new(ssl, tcp).map_err(|e| fail(e.to_string()))?;
        match tokio::time::timeout(CONNECT_TIMEOUT, Pin::new(&mut stream).connect()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(fail(e.to_string())),
            Err(_) => return Err(fail("no answer".into())),
        }
        if let Some(d) = &self.dump {
            d.connection(&peer.host(), Some(stream.ssl()));
        }
        Ok(Conn::new(stream, 64 * 1024))
    }

    /// Exactly what an iPhone sends on /Discover and /Ask: three headers, chunked body
    /// (no Host, Content-Type or Accept-*). iOS 27.2 says "close" on /Discover.
    async fn post(&self, conn: &mut Conn<Stream>, peer: &Peer, path: &str, body: &[u8], timeout: Duration, keep_alive: bool)
        -> std::result::Result<(u16, Vec<u8>), ConnError> {
        let connection = if keep_alive { "keep-alive" } else { "close" };
        let head = http::request_head("POST", path, &[
            ("User-Agent", wire::USER_AGENT.to_string()),
            ("Connection", connection.to_string()),
            ("Transfer-Encoding", "chunked".to_string()),
        ]);
        conn.write_parts(&[&head, &http::chunk(body), http::LAST_CHUNK]).await?;
        if let Some(d) = &self.dump {
            d.request(&peer.host(), "POST", path, serde_json::json!({ "Connection": connection }), body, None, None);
        }
        let (status, head, data) = tokio::time::timeout(timeout, conn.read_response(MAX_SMALL_BODY)).await.map_err(|_| ConnError::Timeout)??;
        let data = match head.headers.get("content-encoding").map(str::to_ascii_lowercase).as_deref() {
            Some("gzip") => inflate(flate2::read::MultiGzDecoder::new(&data[..]))?,
            Some("deflate") => inflate(flate2::read::ZlibDecoder::new(&data[..]))?,
            _ => data,
        };
        if let Some(d) = &self.dump {
            d.response(&peer.host(), path, status, &head.headers, &data);
        }
        Ok((status, data))
    }

    /// /Discover on a connection of its own: fills in the peer's name and model.
    pub async fn discover(&self, peer: &mut Peer) -> Result<()> {
        let mut conn = self.connect(peer).await?;
        let result = self.post(&mut conn, peer, "/Discover", &wire::discover_request(txt_flags()), DISCOVER_TIMEOUT, false).await;
        conn.shutdown().await;
        let (status, body) = result.map_err(|e| SendError::Failed(format!("discover failed: {e}")))?;
        if status != 200 {
            return Err(SendError::Failed(format!("discover returned HTTP {status}")));
        }
        let reply = DiscoverReply::parse(&body).map_err(|_| SendError::Failed("receiver sent an unreadable discover reply".into()))?;
        peer.name = reply.name;
        peer.model = reply.model;
        peer.discovered_at = Some(Instant::now());
        Ok(())
    }

    /// Send files or a link to `peer` (already discovered, or discovered here).
    /// `id` names the transfer in `--json` events (`transfer` asking / sending, `progress`).
    pub async fn send(&self, peer: &mut Peer, what: &What, id: u64) -> Result<()> {
        let plan = match what {
            What::Files(paths) => Some(plan_archive(paths).map_err(|e| SendError::Failed(e.to_string()))?), // stat before asking
            What::Link(_) => None,
        };
        let fresh = peer.available() && peer.discovered_at.is_some_and(|t| t.elapsed() < DISCOVER_FRESH);
        if !fresh {
            let found = retrying(|| async {
                let mut p = peer.clone();
                self.discover(&mut p).await.map(|()| p)
            }).await?;
            *peer = found;
        }
        if !peer.available() {
            return Err(SendError::Failed(format!("{} is not accepting from everyone (set AirDrop to 'Everyone for 10 minutes' on the device)", peer.label())));
        }
        let peer_now = peer.clone();
        let mut conn = retrying(|| self.connect(&peer_now)).await?;
        let transfer_id = uuid4_upper();
        let sender = wire::Sender { name: &self.name, model: &self.model, id: &self.id };
        let (ask, shown) = match what {
            What::Link(url) => (wire::ask_request(&sender, &AskPayload::Link(url), &transfer_id), url.clone()),
            What::Files(paths) => {
                let plan = plan.as_ref().expect("planned above");
                let items = paths.iter().map(|p| AskItem {
                    path: p,
                    size: Some(plan.item_size(p.file_name().map(std::os::unix::ffi::OsStrExt::as_bytes).unwrap_or_default())),
                }).collect();
                let names = paths.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect::<Vec<_>>();
                (wire::ask_request(&sender, &AskPayload::Files(items), &transfer_id), names.join(", "))
            }
        };
        let label = peer.label();
        crate::say!("Waiting for {label} to accept {} ...", clean(&shown));
        crate::events::emit("transfer", serde_json::json!({ "id": id, "dir": "out", "peer": peer.host(), "phase": "asking",
            "total": plan.as_ref().map(|p| p.total_bytes) }));
        match self.post(&mut conn, peer, "/Ask", &ask, ASK_TIMEOUT, true).await {
            Ok((200, _)) => {}
            Ok((status, _)) => return Err(SendError::Declined(format!("{label} declined (HTTP {status})"))),
            Err(ConnError::Timeout) => return Err(SendError::Declined(format!("{label} did not answer"))),
            // An iPhone that declines, has discoverability off, or is rate limiting drops the
            // connection instead of answering. Never retried: that trips its rate limit.
            Err(ConnError::Closed) => return Err(SendError::Declined(format!("{label} declined, or AirDrop is turned off on it (it closed the connection)"))),
            Err(e) => return Err(SendError::Failed(format!("request failed: {e}"))),
        }
        match plan {
            None => {
                crate::say!("Sent link to {label}.");
                conn.shutdown().await;
                Ok(())
            }
            Some(plan) => {
                crate::events::emit("transfer", serde_json::json!({ "id": id, "dir": "out", "peer": peer.host(), "phase": "sending",
                    "total": plan.total_bytes }));
                self.upload(&mut conn, plan, &label, &transfer_id, id).await
            }
        }
    }

    async fn upload(&self, conn: &mut Conn<Stream>, plan: airdrop_proto::cpio::ArchivePlan, label: &str, transfer_id: &str, id: u64)
        -> Result<()> {
        let total = plan.total_bytes;
        let progress = Arc::new(Progress::new(Some(total), format!("Sending to {label}"), Some(id)));
        // The header set and order of a real iOS 26 /Upload. The pseudonym and push token
        // name an Apple account we do not have; receivers carry them without checking.
        let pseudonym = base64::engine::general_purpose::STANDARD_NO_PAD.encode(random_bytes::<16>());
        let head = http::request_head("POST", "/Upload", &[
            ("User-Agent", wire::USER_AGENT.to_string()),
            ("TotalBytes", total.to_string()), // the sum of the files' sizes, as iOS sends it
            ("Content-Type", "application/x-dvzip".to_string()),
            ("SenderPseudonym", format!("pseud:{pseudonym}")),
            ("SenderPushToken", hex(&random_bytes::<32>()).to_uppercase()),
            ("TransferID", transfer_id.to_string()),
            ("Connection", "keep-alive".to_string()),
            ("Transfer-Encoding", "chunked".to_string()),
        ]);
        // Files are read on a thread; blocks come through a short queue.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(8);
        let counter = progress.clone();
        let reader = std::thread::spawn(move || {
            let archive = ArchiveReader::new(plan, Box::new(move |n| counter.add(n)));
            for block in DvzipBlocks::new(archive) {
                let stop = block.is_err();
                if tx.blocking_send(block).is_err() || stop {
                    return;
                }
            }
        });
        let stall = self.stall;
        // Experiments E5.8/E5.9 (what an iPhone receiver does when the sender stops or is late):
        // stop the upload at this share of the bytes; wait this long before starting it.
        let cancel_at = crate::util::env("AIRDROP_SEND_CANCEL_AT").and_then(|v| v.parse::<f64>().ok())
            .filter(|p| (0.0..100.0).contains(p)).map(|p| (total as f64 * p / 100.0) as u64);
        let delay = crate::util::env_secs("AIRDROP_SEND_UPLOAD_DELAY", 0.0);
        /// Why the body did not go out: our files, the connection, or the experiment above.
        enum Stopped {
            Local(std::io::Error),
            Conn(ConnError),
            Experiment(u64),
        }
        let sent: std::result::Result<(), Stopped> = async {
            if !delay.is_zero() {
                crate::say!("Experiment: starting the upload {:.0} s late.", delay.as_secs_f64());
                tokio::time::sleep(delay).await;
            }
            tokio::time::timeout(stall, conn.write_all(&head)).await.map_err(|_| Stopped::Conn(ConnError::Timeout))?.map_err(Stopped::Conn)?;
            while let Some(block) = rx.recv().await {
                let block = block.map_err(Stopped::Local)?;
                // One buffer, so OpenSSL makes full 16 KiB records: separate writes for the
                // size line and the CRLF become tiny records, and with TCP_NODELAY tiny frames
                // on air, where airtime is what limits a send.
                tokio::time::timeout(stall, conn.write_all(&http::chunk(&block))).await
                    .map_err(|_| Stopped::Conn(ConnError::Timeout))?.map_err(Stopped::Conn)?;
                if cancel_at.is_some_and(|at| progress.done() >= at) {
                    return Err(Stopped::Experiment(progress.done()));
                }
            }
            tokio::time::timeout(stall, conn.write_all(http::LAST_CHUNK)).await.map_err(|_| Stopped::Conn(ConnError::Timeout))?.map_err(Stopped::Conn)?;
            Ok(())
        }.await;
        drop(rx);
        let _ = reader.join();
        let response = match sent {
            Ok(()) => tokio::time::timeout(UPLOAD_DONE_TIMEOUT, conn.read_response(MAX_SMALL_BODY)).await.map_err(|_| ConnError::Timeout).and_then(|r| r),
            Err(Stopped::Local(e)) => {
                progress.finish();
                return Err(SendError::Failed(e.to_string()));
            }
            Err(Stopped::Experiment(at)) => {
                progress.finish();
                conn.shutdown().await;
                return Err(SendError::Failed(format!("experiment: upload stopped on purpose after {} of {}", human(at as f64), human(total as f64))));
            }
            Err(Stopped::Conn(e)) => Err(e),
        };
        let status = match response {
            Ok((status, head, body)) => {
                if let Some(d) = &self.dump {
                    d.response(label, "/Upload", status, &head.headers, &body);
                }
                status
            }
            Err(ConnError::Timeout) => {
                progress.finish();
                return Err(SendError::Failed(if progress.done() < total {
                    format!("upload stalled: {label} stopped receiving (no progress for {:.0} s; is it still unlocked and nearby?)", stall.as_secs_f64())
                } else {
                    "receiver did not confirm the upload".into()
                }));
            }
            Err(e) => {
                // The receiver may have answered and hung up mid-stream: report its status if we can.
                progress.finish();
                return match tokio::time::timeout(Duration::from_secs(2), conn.read_response(MAX_SMALL_BODY)).await {
                    Ok(Ok((s @ (401 | 403), _, _))) => Err(SendError::Declined(format!("{label} declined (HTTP {s})"))),
                    Ok(Ok((s, _, _))) => Err(SendError::Failed(format!("receiver stopped the upload (HTTP {s})"))),
                    _ => Err(SendError::Failed(format!("upload interrupted: {e}"))),
                };
            }
        };
        let elapsed = progress.finish();
        conn.shutdown().await;
        match status {
            200 => {
                let rate = total as f64 / elapsed;
                crate::say!("Sent {} to {label} in {elapsed:.1}s ({}/s).", human(total as f64), human(rate));
                Ok(())
            }
            // Early-ask receivers say yes to /Ask at once and refuse here instead.
            401 | 403 => Err(SendError::Declined(format!("{label} declined (HTTP {status})"))),
            _ => Err(SendError::Failed(format!("receiver rejected the upload (HTTP {status})"))),
        }
    }
}

pub enum What {
    Files(Vec<PathBuf>),
    Link(String),
}

/// The flags we advertise and send; `AIRDROP_TXT_FLAGS` (e.g. 0x3FB) overrides them (E3.3).
pub fn txt_flags() -> u64 {
    crate::util::env("AIRDROP_TXT_FLAGS").and_then(|v| crate::util::parse_int(&v)).unwrap_or(wire::RECEIVER_FLAGS)
}

fn inflate(r: impl Read) -> std::result::Result<Vec<u8>, ConnError> {
    let mut out = Vec::new();
    r.take(MAX_REPLY + 1).read_to_end(&mut out).map_err(|e| ConnError::Io(std::io::Error::other(format!("bad compressed reply: {e}"))))?;
    if out.len() as u64 > MAX_REPLY {
        return Err(ConnError::Io(std::io::Error::other("reply too large once decompressed")));
    }
    Ok(out)
}

/// A connect or /Discover again when it fails: the receiver was found a moment ago, so a
/// failure is usually it being away on another channel for a few seconds, not gone.
async fn retrying<T, F, Fut>(mut make: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 1;
    loop {
        match make().await {
            Err(SendError::Failed(_)) if attempt < CONNECT_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            other => return other,
        }
    }
}
