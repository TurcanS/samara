//! `--dump DIR`: record what Apple devices send (and answer), for reverse engineering and
//! for checking new iOS versions against docs/PROTOCOL.md. One folder per session, one
//! numbered JSON file per request; binary values over 64 bytes (the JPEG 2000 `FileIcon`,
//! signed `SenderRecordData`) go to their own files next to it.
//!
//! Privacy: dumps hold device names, file names, a preview image and Apple-signed
//! identity data. Share them with care.

use crate::util::{hex, iso_now, stamp};
use airdrop_proto::extract::Member;
use airdrop_proto::http::Headers;
use plist::Value;
use serde_json::{json, Map, Value as Json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct Dump {
    dir: PathBuf,
    n: AtomicU32,
    conn: AtomicU32,
}

fn ext_for(key: &str) -> &'static str {
    match key {
        "FileIcon" => ".jp2",
        "SenderRecordData" | "ReceiverRecordData" => ".der",
        _ => ".bin",
    }
}

/// A plist key as part of a file name: letters, digits, `_` and `-` only.
fn safe(key: &str) -> String {
    let s: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(64).collect();
    if s.is_empty() { "data".into() } else { s }
}

pub fn headers_json(h: &Headers) -> Json {
    Json::Object(h.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
}

impl Dump {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let dir = crate::util::expand_user(&root.to_string_lossy()).join(stamp());
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, n: AtomicU32::new(1), conn: AtomicU32::new(1) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn next(&self) -> u32 {
        self.n.fetch_add(1, Ordering::Relaxed)
    }

    /// Never lets a full disk or a bad name stop a transfer.
    fn write(&self, name: &str, bytes: &[u8]) {
        let _ = std::fs::write(self.dir.join(name), bytes);
    }

    fn save(&self, stem: &str, rec: &Json) {
        let text = serde_json::to_string_pretty(rec).unwrap_or_default();
        self.write(&format!("{stem}.json"), text.as_bytes());
    }

    fn jsonable(&self, v: &Value, stem: &str, key: &str, used: &mut u32) -> Json {
        match v {
            Value::Dictionary(d) => Json::Object(d.iter().map(|(k, v)| (k.clone(), self.jsonable(v, stem, k, used))).collect()),
            Value::Array(a) => Json::Array(a.iter().map(|v| self.jsonable(v, stem, key, used)).collect()),
            Value::Data(b) if b.len() <= 64 => json!({ "bytes": b.len(), "hex": hex(b) }),
            Value::Data(b) => {
                *used += 1;
                // a repeated key (in a list, a nested dict) gets its own file
                let suffix = if *used > 1 { format!("-{used}") } else { String::new() };
                let name = format!("{stem}-{}{suffix}{}", safe(if key.is_empty() { "data" } else { key }), ext_for(key));
                self.write(&name, b);
                json!({ "bytes": b.len(), "file": name, "head_hex": hex(&b[..16.min(b.len())]) })
            }
            Value::Boolean(b) => json!(b),
            Value::Integer(i) => i.as_signed().map_or_else(|| json!(i.as_unsigned()), |s| json!(s)),
            Value::Real(r) => json!(r),
            Value::String(s) => json!(s),
            Value::Date(d) => json!(d.to_xml_format()),
            other => json!(format!("{other:?}")),
        }
    }

    fn body(&self, rec: &mut Map<String, Json>, stem: &str, body: &[u8]) {
        rec.insert("body_bytes".into(), json!(body.len()));
        if body.is_empty() {
            rec.insert("plist".into(), json!({}));
            rec.insert("plist_format".into(), json!("xml"));
            return;
        }
        match Value::from_reader(std::io::Cursor::new(body)) {
            Ok(v) => {
                let mut used = 0;
                rec.insert("plist".into(), self.jsonable(&v, stem, "", &mut used));
                rec.insert("plist_format".into(), json!(if body.starts_with(b"bplist00") { "binary" } else { "xml" }));
            }
            Err(e) => {
                rec.insert("plist_error".into(), json!(e.to_string()));
                self.write(&format!("{stem}-body.bin"), body);
            }
        }
    }

    /// A request we received (receiver) or sent (sender).
    #[allow(clippy::too_many_arguments)]
    pub fn request(&self, peer: &str, method: &str, path: &str, headers: Json, body: &[u8], status: Option<u16>, note: Option<&str>) {
        let stem = format!("{:03}{}", self.next(), path.replace('/', "-"));
        let mut rec = Map::new();
        rec.insert("time".into(), json!(iso_now()));
        rec.insert("peer".into(), json!(peer));
        rec.insert("request".into(), json!(format!("{method} {path}")));
        rec.insert("headers".into(), headers);
        if let Some(s) = status {
            rec.insert("our_status".into(), json!(s));
        }
        if let Some(n) = note {
            rec.insert("note".into(), json!(n));
        }
        self.body(&mut rec, &stem, body);
        self.save(&stem, &Json::Object(rec));
    }

    /// What a receiver answered us (`send/find --dump`).
    pub fn response(&self, peer: &str, path: &str, status: u16, headers: &Headers, body: &[u8]) {
        let stem = format!("{:03}{}-reply", self.next(), path.replace('/', "-"));
        let mut rec = Map::new();
        rec.insert("time".into(), json!(iso_now()));
        rec.insert("peer".into(), json!(peer));
        rec.insert("reply_to".into(), json!(format!("POST {path}")));
        rec.insert("status".into(), json!(status));
        rec.insert("headers".into(), headers_json(headers));
        self.body(&mut rec, &stem, body);
        self.save(&stem, &Json::Object(rec));
    }

    /// One resolved mDNS service: the receiver's TXT record.
    pub fn service(&self, name: &str, host: &str, port: u16, txt: &[(String, String)]) {
        let stem = format!("{:03}-mdns", self.next());
        let txt: Map<String, Json> = txt.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        self.save(&stem, &json!({ "time": iso_now(), "service": name, "host": host, "port": port, "txt": txt }));
    }

    /// TLS facts of one connection.
    pub fn connection(&self, peer: &str, ssl: Option<&openssl::ssl::SslRef>) {
        let stem = format!("tls-{:03}", self.conn.fetch_add(1, Ordering::Relaxed));
        let tls = ssl.map(|s| {
            let (mut facts, cert) = crate::tls::describe(s);
            if let Some(der) = cert {
                let name = format!("{stem}-peer-cert.der");
                self.write(&name, &der);
                facts["peer_certificate_file"] = json!(name);
            }
            facts
        });
        self.save(&stem, &json!({ "time": iso_now(), "peer": peer, "tls": tls }));
    }

    pub fn upload(&self, peer: &str, headers: &Headers, container: &str, members: &[Member], status: u16, received: u64) {
        let stem = format!("{:03}-Upload", self.next());
        let members: Vec<Json> = members.iter().map(|m| json!({ "name": m.name, "mode": format!("0o{:o}", m.mode), "size": m.size })).collect();
        self.save(&stem, &json!({
            "time": iso_now(), "peer": peer, "request": "POST /Upload", "headers": headers_json(headers),
            "container": container, "body_bytes_received": received, "our_status": status, "archive_members": members,
        }));
    }
}
