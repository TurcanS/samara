//! Minimal HTTP/1.1 framing for AirDrop, without I/O: a head parser and a body decoder
//! that are fed bytes, and writers that return bytes. The caller owns the socket and the
//! buffer, so leftover bytes after a body stay there for the next request.

use std::fmt;

/// Discover/Ask plists are small; refuse anything absurd.
pub const MAX_SMALL_BODY: usize = 16 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    HeadTooLarge,
    Malformed(String),
    BadChunk,
    BadLength,
    TooLarge,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeadTooLarge => f.write_str("headers too large"),
            Self::Malformed(what) => write!(f, "malformed HTTP: {what}"),
            Self::BadChunk => f.write_str("bad chunk framing"),
            Self::BadLength => f.write_str("bad Content-Length"),
            Self::TooLarge => f.write_str("body too large"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Lower-cased names in arrival order; a repeated name is joined with ", ".
#[derive(Debug, Clone, Default)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    fn add(&mut self, name: String, value: String) {
        match self.0.iter_mut().find(|(k, _)| *k == name) {
            Some((_, v)) => {
                v.push_str(", ");
                v.push_str(&value);
            }
            None => self.0.push((name, value)),
        }
    }

    pub fn is_chunked(&self) -> bool {
        self.get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    }
}

#[derive(Debug, Clone)]
pub struct Head {
    pub start: String,
    pub headers: Headers,
}

impl Head {
    /// (method, path without the query) of a request line.
    pub fn request_line(&self) -> Option<(&str, &str)> {
        let mut parts = self.start.split(' ');
        let method = parts.next().filter(|m| !m.is_empty())?;
        let target = parts.next()?;
        Some((method, target.split('?').next().unwrap_or(target)))
    }

    /// The status code of a response line.
    pub fn status(&self) -> Option<u16> {
        self.start.split(' ').nth(1)?.parse().ok()
    }
}

/// Find a complete head at the start of `buf`: `Ok(Some((head, length)))` once
/// `\r\n\r\n` is there, `Ok(None)` while more bytes are needed.
pub fn parse_head(buf: &[u8], limit: usize) -> Result<Option<(Head, usize)>, HttpError> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > limit { Err(HttpError::HeadTooLarge) } else { Ok(None) };
    };
    if end > limit {
        return Err(HttpError::HeadTooLarge);
    }
    // latin-1: every byte is one char
    let text: String = buf[..end].iter().map(|&b| b as char).collect();
    let mut lines = text.split("\r\n");
    let start = lines.next().unwrap_or_default().to_string();
    let mut headers = Headers::default();
    for line in lines.filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').ok_or_else(|| HttpError::Malformed(format!("header line {line:?}")))?;
        headers.add(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    Ok(Some((Head { start, headers }, end + 4)))
}

/// What one step of the body decoder found.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// `n` body bytes start at offset `at` of the input.
    Data { at: usize, n: usize },
    /// Nothing complete in the input; feed more.
    NeedMore,
    /// The body is complete.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    /// Content-Length bytes still to come.
    Length(u64),
    /// Waiting for a chunk-size line.
    ChunkSize,
    /// Bytes of the current chunk still to come.
    ChunkData(u64),
    /// The CRLF after a chunk.
    ChunkEnd,
    /// Trailer lines after the last chunk, until an empty line.
    Trailers,
    Done,
}

/// Decodes a chunked or Content-Length body. Each `step` consumes a prefix of the input
/// and returns how many bytes it consumed.
#[derive(Debug)]
pub struct BodyDecoder(Body);

impl BodyDecoder {
    pub fn new(headers: &Headers) -> Result<Self, HttpError> {
        if headers.is_chunked() {
            return Ok(Self(Body::ChunkSize));
        }
        let length = match headers.get("content-length") {
            None => 0,
            Some(v) => v.trim().parse::<u64>().map_err(|_| HttpError::BadLength)?,
        };
        Ok(Self(if length == 0 { Body::Done } else { Body::Length(length) }))
    }

    pub fn is_done(&self) -> bool {
        self.0 == Body::Done
    }

    /// Bytes still expected, when the framing knows (Content-Length).
    pub fn remaining(&self) -> Option<u64> {
        match self.0 {
            Body::Length(n) => Some(n),
            Body::Done => Some(0),
            _ => None,
        }
    }

    pub fn step(&mut self, input: &[u8]) -> Result<(usize, Step), HttpError> {
        let mut used = 0;
        loop {
            let rest = &input[used..];
            match self.0 {
                Body::Done => return Ok((used, Step::Done)),
                Body::Length(left) | Body::ChunkData(left) => {
                    if rest.is_empty() {
                        return Ok((used, Step::NeedMore));
                    }
                    let n = rest.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let left = left - n as u64;
                    self.0 = match self.0 {
                        Body::Length(_) if left == 0 => Body::Done,
                        Body::Length(_) => Body::Length(left),
                        _ if left == 0 => Body::ChunkEnd,
                        _ => Body::ChunkData(left),
                    };
                    return Ok((used + n, Step::Data { at: used, n }));
                }
                Body::ChunkEnd => {
                    if rest.len() < 2 {
                        return Ok((used, Step::NeedMore));
                    }
                    if &rest[..2] != b"\r\n" {
                        return Err(HttpError::BadChunk);
                    }
                    used += 2;
                    self.0 = Body::ChunkSize;
                }
                Body::ChunkSize | Body::Trailers => {
                    let Some(eol) = rest.windows(2).position(|w| w == b"\r\n") else {
                        if rest.len() > 4096 {
                            return Err(HttpError::BadChunk);
                        }
                        return Ok((used, Step::NeedMore));
                    };
                    let line = &rest[..eol];
                    used += eol + 2;
                    if self.0 == Body::Trailers {
                        if line.is_empty() {
                            self.0 = Body::Done;
                        }
                        continue;
                    }
                    let size = line.split(|&b| b == b';').next().unwrap_or_default();
                    let size = std::str::from_utf8(size).map_err(|_| HttpError::BadChunk)?.trim();
                    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(HttpError::BadChunk);
                    }
                    let size = u64::from_str_radix(size, 16).map_err(|_| HttpError::BadChunk)?;
                    self.0 = if size == 0 { Body::Trailers } else { Body::ChunkData(size) };
                }
            }
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

fn head(start: &str, headers: &[(&str, String)]) -> Vec<u8> {
    let mut out = String::from(start);
    for (k, v) in headers {
        out.push_str("\r\n");
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
    }
    out.push_str("\r\n\r\n");
    out.into_bytes()
}

/// A request head, headers written in the given order.
pub fn request_head(method: &str, path: &str, headers: &[(&str, String)]) -> Vec<u8> {
    head(&format!("{method} {path} HTTP/1.1"), headers)
}

/// A complete response. `keep_alive: false` says the connection closes after it.
pub fn response(status: u16, body: &[u8], keep_alive: bool) -> Vec<u8> {
    let mut headers = vec![
        ("Content-Length", body.len().to_string()),
        ("Connection", if keep_alive { "keep-alive" } else { "close" }.to_string()),
    ];
    if !body.is_empty() {
        headers.push(("Content-Type", "application/octet-stream".to_string()));
    }
    let mut out = head(&format!("HTTP/1.1 {status} {}", reason(status)), &headers);
    out.extend_from_slice(body);
    out
}

pub const CONTINUE: &[u8] = b"HTTP/1.1 100 Continue\r\n\r\n";
pub const LAST_CHUNK: &[u8] = b"0\r\n\r\n";

/// The size line that starts a chunk of `len` bytes (not 0: that is the last chunk); the
/// data and a CRLF follow it.
pub fn chunk_header(len: usize) -> Vec<u8> {
    format!("{len:X}\r\n").into_bytes()
}

/// One chunk of a chunked body (`data` must not be empty: that is the last chunk).
pub fn chunk(data: &[u8]) -> Vec<u8> {
    let mut out = chunk_header(data.len());
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}
