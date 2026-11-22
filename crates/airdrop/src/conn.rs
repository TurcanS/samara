//! One HTTP/1.1 connection over TLS: our read buffer, the protocol crate's parser.
//! `fill` is the only await on the read side and is cancel-safe, so callers may race it
//! against timers and decisions in `select!` without losing bytes.

use airdrop_proto::http::{self, BodyDecoder, Head, HttpError, Step};
use bytes::{Buf, BytesMut};
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const READ_CHUNK: usize = 1 << 20;

#[derive(Debug)]
pub enum ConnError {
    /// The peer closed (or reset) the connection.
    Closed,
    Timeout,
    Http(HttpError),
    Io(io::Error),
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("connection closed"),
            Self::Timeout => f.write_str("timed out"),
            Self::Http(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConnError {}

impl From<HttpError> for ConnError {
    fn from(e: HttpError) -> Self {
        Self::Http(e)
    }
}

/// OpenSSL reports a peer that hangs up without close_notify (or resets mid-record) as an
/// SSL error of kind `Other`, not as `UnexpectedEof`; it is still the peer closing.
fn tls_hangup(e: &io::Error) -> bool {
    use openssl::ssl::{Error, ErrorCode};
    let Some(ssl) = e.get_ref().and_then(|i| i.downcast_ref::<Error>()) else { return false };
    match ssl.code() {
        ErrorCode::SYSCALL => ssl.io_error().is_none_or(|io| matches!(io.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof)),
        ErrorCode::SSL => ssl.ssl_error().is_some_and(|st| st.errors().iter()
            .any(|x| x.reason().is_some_and(|r| r.contains("unexpected eof")))),
        ErrorCode::ZERO_RETURN => true,
        _ => false,
    }
}

impl From<io::Error> for ConnError {
    fn from(e: io::Error) -> Self {
        use io::ErrorKind::*;
        match e.kind() {
            ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof | NotConnected => Self::Closed,
            _ if tls_hangup(&e) => Self::Closed,
            _ => Self::Io(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, ConnError>;

pub struct Conn<S> {
    io: S,
    buf: BytesMut,
    head_limit: usize,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    pub fn new(io: S, head_limit: usize) -> Self {
        Self { io, buf: BytesMut::with_capacity(64 * 1024), head_limit }
    }

    /// Read more bytes into the buffer. Cancel-safe. Returns false at EOF.
    pub async fn fill(&mut self) -> Result<bool> {
        self.buf.reserve(READ_CHUNK.min(256 * 1024));
        Ok(self.io.read_buf(&mut self.buf).await? > 0)
    }

    /// The next request or response head; `None` on a clean EOF between messages.
    pub async fn read_head(&mut self) -> Result<Option<Head>> {
        loop {
            if let Some((head, used)) = http::parse_head(&self.buf, self.head_limit)? {
                self.buf.advance(used);
                return Ok(Some(head));
            }
            if !self.fill().await? {
                return if self.buf.iter().all(u8::is_ascii_whitespace) { Ok(None) } else { Err(ConnError::Closed) };
            }
        }
    }

    /// Decode body bytes already in the buffer (no I/O): `Some(chunk)`, or `None` when the
    /// buffer holds nothing more of the body (check `dec.is_done()`).
    pub fn take_buffered(&mut self, dec: &mut BodyDecoder) -> Result<Option<Vec<u8>>> {
        let (used, step) = dec.step(&self.buf)?;
        let chunk = match step {
            Step::Data { at, n } => Some(self.buf[at..at + n].to_vec()),
            Step::NeedMore | Step::Done => None,
        };
        self.buf.advance(used);
        Ok(chunk)
    }

    /// The next piece of a body, `None` once it is complete.
    pub async fn body_chunk(&mut self, dec: &mut BodyDecoder) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(chunk) = self.take_buffered(dec)? {
                return Ok(Some(chunk));
            }
            if dec.is_done() {
                return Ok(None);
            }
            if !self.fill().await? {
                return Err(ConnError::Closed);
            }
        }
    }

    /// A whole small body (Discover/Ask plists), at most `limit` bytes.
    pub async fn read_body(&mut self, head: &Head, limit: usize) -> Result<Vec<u8>> {
        let mut dec = BodyDecoder::new(&head.headers)?;
        let mut body = Vec::new();
        while let Some(chunk) = self.body_chunk(&mut dec).await? {
            body.extend_from_slice(&chunk);
            if body.len() > limit {
                return Err(HttpError::TooLarge.into());
            }
        }
        Ok(body)
    }

    pub async fn read_body_within(&mut self, head: &Head, limit: usize, timeout: Duration) -> Result<Vec<u8>> {
        tokio::time::timeout(timeout, self.read_body(head, limit)).await.map_err(|_| ConnError::Timeout)?
    }

    /// A response: its head (100 Continue skipped) and its whole body.
    pub async fn read_response(&mut self, limit: usize) -> Result<(u16, Head, Vec<u8>)> {
        loop {
            let head = self.read_head().await?.ok_or(ConnError::Closed)?;
            let status = head.status().ok_or_else(|| HttpError::Malformed(format!("status line {:?}", head.start)))?;
            if status == 100 {
                continue;
            }
            let body = self.read_body(&head, limit).await?;
            return Ok((status, head, body));
        }
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.io.write_all(data).await?;
        self.io.flush().await?;
        Ok(())
    }

    /// Several pieces in one flush.
    pub async fn write_parts(&mut self, parts: &[&[u8]]) -> Result<()> {
        for p in parts {
            self.io.write_all(p).await?;
        }
        self.io.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), self.io.shutdown()).await;
    }

    /// Close after answering a peer that is still sending: shut our side, then read and
    /// discard what it sends for up to `within`. Closing with unread bytes queued makes the
    /// kernel send a reset, which can destroy the answer before the peer has read it.
    pub async fn linger(&mut self, within: Duration) {
        self.shutdown().await;
        let _ = tokio::time::timeout(within, async {
            loop {
                self.buf.clear();
                if !matches!(self.fill().await, Ok(true)) {
                    break;
                }
            }
        }).await;
    }
}
