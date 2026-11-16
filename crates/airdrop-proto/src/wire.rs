//! AirDrop message bodies (binary plists), constants and the UTI map.
//!
//! Policy (environment knobs, configuration) stays with the caller: every builder takes
//! its inputs as arguments.

use plist::{Dictionary, Value};
use std::path::Path;

pub const SERVICE_TYPE: &str = "_airdrop._tcp.local.";

// TXT "flags" bits advertised by a receiver.
pub const SUPPORTS_URL: u64 = 0x01;
pub const SUPPORTS_DVZIP: u64 = 0x02;
pub const SUPPORTS_MIXED_TYPES: u64 = 0x08;
pub const SUPPORTS_DISCOVER_MAYBE: u64 = 0x80;
/// 0x8B: what we implement as a receiver; also our /Discover `DeviceSupportFlags`.
pub const RECEIVER_FLAGS: u64 = SUPPORTS_URL | SUPPORTS_DVZIP | SUPPORTS_MIXED_TYPES | SUPPORTS_DISCOVER_MAYBE;

pub const USER_AGENT: &str = "AirDrop/1.0";
pub const BUNDLE_ID: &str = "com.apple.finder";
/// Apple receivers listen here; ours listens on 8771 by default.
pub const APPLE_PORT: u16 = 8770;

const UTI: &[(&str, &str)] = &[
    ("jpg", "public.jpeg"), ("jpeg", "public.jpeg"), ("png", "public.png"), ("gif", "com.compuserve.gif"),
    ("heic", "public.heic"), ("heif", "public.heif"), ("tif", "public.tiff"), ("tiff", "public.tiff"),
    ("webp", "org.webmproject.webp"), ("bmp", "com.microsoft.bmp"), ("svg", "public.svg-image"),
    ("mov", "com.apple.quicktime-movie"), ("mp4", "public.mpeg-4"), ("m4v", "com.apple.m4v-video"),
    ("avi", "public.avi"), ("mkv", "org.matroska.mkv"), ("webm", "org.webmproject.webm"),
    ("mp3", "public.mp3"), ("m4a", "com.apple.m4a-audio"), ("aac", "public.aac-audio"),
    ("wav", "com.microsoft.waveform-audio"), ("flac", "org.xiph.flac"), ("ogg", "org.xiph.ogg"),
    ("pdf", "com.adobe.pdf"), ("txt", "public.plain-text"), ("md", "net.daringfireball.markdown"),
    ("rtf", "public.rtf"), ("html", "public.html"), ("htm", "public.html"), ("json", "public.json"),
    ("xml", "public.xml"), ("csv", "public.comma-separated-values-text"), ("vcf", "public.vcard"),
    ("ics", "com.apple.ical.ics"), ("zip", "public.zip-archive"), ("gz", "org.gnu.gnu-zip-archive"),
    ("tar", "public.tar-archive"), ("7z", "org.7-zip.7-zip-archive"), ("epub", "org.idpf.epub-container"),
    ("doc", "com.microsoft.word.doc"), ("xls", "com.microsoft.excel.xls"),
    ("ppt", "com.microsoft.powerpoint.ppt"),
    ("docx", "org.openxmlformats.wordprocessingml.document"),
    ("xlsx", "org.openxmlformats.spreadsheetml.sheet"),
    ("pptx", "org.openxmlformats.presentationml.presentation"),
    ("pages", "com.apple.iwork.pages.sffpages"), ("numbers", "com.apple.iwork.numbers.sffnumbers"),
    ("key", "com.apple.iwork.keynote.sffkey"), ("ipa", "com.apple.itunes.ipa"),
];

/// The UTI a Mac would announce for `path` (folders: `public.folder`).
pub fn uti_for(path: &Path) -> &'static str {
    if path.is_dir() {
        return "public.folder";
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    UTI.iter().find(|(e, _)| *e == ext).map_or("public.data", |(_, u)| u)
}

#[derive(Debug)]
pub struct PlistError(pub String);

impl std::fmt::Display for PlistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlistError {}

/// Keys sorted at every level, as Python's plistlib writes them (and as the old client did).
fn sorted(v: Value) -> Value {
    match v {
        Value::Dictionary(d) => {
            let mut items: Vec<(String, Value)> = d.into_iter().collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Dictionary(items.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// A dictionary as a binary plist.
pub fn dumps(d: Dictionary) -> Vec<u8> {
    let mut out = Vec::new();
    sorted(Value::Dictionary(d)).to_writer_binary(&mut out).expect("writing a plist to memory cannot fail");
    out
}

/// A body as a dictionary; an empty body is an empty dictionary.
pub fn loads(body: &[u8]) -> Result<Dictionary, PlistError> {
    if body.is_empty() {
        return Ok(Dictionary::new());
    }
    match Value::from_reader(std::io::Cursor::new(body)) {
        Ok(Value::Dictionary(d)) => Ok(d),
        Ok(_) => Err(PlistError("plist body is not a dictionary".into())),
        Err(e) => Err(PlistError(e.to_string())),
    }
}

fn dict<const N: usize>(items: [(&str, Value); N]) -> Dictionary {
    items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

// ---- requests we send ------------------------------------------------------------

/// Every Apple sender's /Discover carries `DeviceSupportFlags`, the bitmask a receiver
/// publishes as TXT `flags`; ours is the receiver flags we implement.
pub fn discover_request(flags: u64) -> Vec<u8> {
    dumps(dict([("DeviceSupportFlags", Value::Integer(flags.into()))]))
}

/// One top-level item of a file /Ask.
pub struct AskItem<'a> {
    pub path: &'a Path,
    /// Bytes in it (a folder counts its files); `None` leaves `FileSize` out.
    pub size: Option<u64>,
}

pub enum AskPayload<'a> {
    Files(Vec<AskItem<'a>>),
    Link(&'a str),
}

pub struct Sender<'a> {
    pub name: &'a str,
    pub model: &'a str,
    pub id: &'a str,
}

/// The /Ask an iPhone on iOS 27.2 sends (measured, docs/PROTOCOL.md §9.1). iOS 26+ binds the
/// /Upload to this /Ask by `transfer_id`, repeated in the /Upload `TransferID` header.
pub fn ask_request(sender: &Sender, payload: &AskPayload, transfer_id: &str) -> Vec<u8> {
    let mut d = dict([
        ("SenderComputerName", sender.name.into()),
        ("BundleID", BUNDLE_ID.into()),
        ("SenderModelName", sender.model.into()),
        ("SenderID", sender.id.into()),
        ("ConvertMediaFormats", false.into()),
        ("TransferID", Value::Dictionary(dict([("id", transfer_id.into())]))),
    ]);
    match payload {
        AskPayload::Link(url) => {
            d.insert("TransferType".into(), Value::Dictionary(dict([("links", Value::Dictionary(Dictionary::new()))])));
            d.insert("Items".into(), Value::Array(vec![(*url).into()]));
            d.insert("Files".into(), Value::Array(vec![]));
        }
        AskPayload::Files(items) => {
            d.insert("TransferType".into(), Value::Dictionary(dict([("files", Value::Dictionary(Dictionary::new()))])));
            d.insert("Items".into(), Value::Array(vec![]));
            let files = items.iter().map(|item| {
                let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut f = dict([
                    ("FileName", name.as_str().into()),
                    ("FileType", uti_for(item.path).into()),
                    ("FileBomPath", format!("./{name}").into()),
                    ("FileIsDirectory", item.path.is_dir().into()),
                    ("ShouldConvertMediaFormats", false.into()),
                ]);
                if let Some(size) = item.size {
                    f.insert("FileSize".into(), Value::Integer(size.into()));
                }
                Value::Dictionary(f)
            });
            d.insert("Files".into(), Value::Array(files.collect()));
        }
    }
    dumps(d)
}

// ---- responses we send as a receiver --------------------------------------------

/// `{"Version": 1}` exactly as the old client sent it (Python json, with the space).
pub const DEFAULT_MEDIA_CAPS: &[u8] = br#"{"Version": 1}"#;
/// "We can open HEIF photos and HEVC video": the shape an iPhone 17 (iOS 27.2) reports
/// about itself in its own /Discover reply (measured 2026-11-11), reduced to container and
/// codec. Without it the iPhone converts every photo to JPEG before sending. Left out on
/// purpose: the Live Photo / asset-bundle and adjustment keys (they ask for bundles).
pub const ORIGINAL_MEDIA_CAPS: &[u8] = br#"{"Version":3,"Codecs":{"hvc1":{"Profiles":{"VTSupportedProfiles":[1,2],"VTPerProfileSupport":{"1":{"VTMaxDecodeLevel":186,"VTIsHardwareAccelerated":true},"2":{"VTMaxDecodeLevel":186,"VTIsHardwareAccelerated":true}}},"CodecSupport":{"VTCodecSupportDict":{"hvc1":{"VTSupportedProfiles":[1,2],"VTPerProfileSupport":{"1":{"VTMaxDecodeLevel":186,"VTIsHardwareAccelerated":true},"2":{"VTMaxDecodeLevel":186,"VTIsHardwareAccelerated":true}}}}}}},"ContainerFormats":{"public.heif-standard":{"HeifSubtypes":["public.heic","public.heif"]}}}"#;

pub fn discover_response(name: &str, model: &str, media_caps: Option<&[u8]>) -> Vec<u8> {
    let mut d = dict([("ReceiverComputerName", name.into()), ("ReceiverModelName", model.into())]);
    if let Some(caps) = media_caps {
        d.insert("ReceiverMediaCapabilities".into(), Value::Data(caps.to_vec()));
    }
    dumps(d)
}

pub fn ask_response(name: &str, model: &str, contact_exchange: bool) -> Vec<u8> {
    let mut d = dict([("ReceiverComputerName", name.into()), ("ReceiverModelName", model.into())]);
    if contact_exchange {
        d.insert("SupportsContactExchange".into(), true.into()); // experiment E5.4
    }
    dumps(d)
}

/// What a /Discover reply says about the receiver. Non-string values count as missing.
pub struct DiscoverReply {
    pub name: Option<String>,
    pub model: Option<String>,
}

impl DiscoverReply {
    pub fn parse(body: &[u8]) -> Result<Self, PlistError> {
        let d = loads(body)?;
        let text = |k: &str| d.get(k).and_then(Value::as_string).filter(|s| !s.is_empty()).map(str::to_string);
        Ok(Self { name: text("ReceiverComputerName"), model: text("ReceiverModelName") })
    }
}

// ---- a parsed incoming /Ask -----------------------------------------------------

#[derive(Debug, Clone)]
pub struct AskFile {
    pub name: String,
    pub uti: String,
    pub is_dir: bool,
    /// The item's place in the upload archive.
    pub bom_path: String,
}

#[derive(Debug, Clone)]
pub struct AskInfo {
    /// `None` when the sender sent no usable name (shown as "Unknown device").
    pub sender_name: Option<String>,
    pub sender_model: String,
    pub sender_id: String,
    pub files: Vec<AskFile>,
    /// Links, taken from `Items` only for a link transfer (or one with no files).
    pub urls: Vec<String>,
    pub transfer_id: Option<String>,
}

impl AskInfo {
    pub fn parse(body: &[u8]) -> Result<Self, PlistError> {
        let d = loads(body)?;
        let text = |d: &Dictionary, k: &str| d.get(k).and_then(Value::as_string).map(str::to_string);
        let files: Vec<AskFile> = d.get("Files").and_then(Value::as_array).map(|a| {
            a.iter().filter_map(Value::as_dictionary).map(|f| AskFile {
                name: text(f, "FileName").unwrap_or_else(|| "?".into()),
                uti: text(f, "FileType").unwrap_or_else(|| "public.data".into()),
                is_dir: f.get("FileIsDirectory").and_then(Value::as_boolean).unwrap_or(false),
                bom_path: text(f, "FileBomPath").unwrap_or_default(),
            }).collect()
        }).unwrap_or_default();
        let kind = d.get("TransferType").and_then(Value::as_dictionary);
        let is_links = kind.is_some_and(|k| k.contains_key("links"));
        let is_files = kind.is_some_and(|k| k.contains_key("files"));
        // Items are links only in a link transfer; older senders leave TransferType out and
        // send no files with a link.
        let urls = if is_links || (!is_files && files.is_empty()) {
            d.get("Items").and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_string).map(str::to_string).collect())
                .unwrap_or_default()
        } else {
            vec![]
        };
        Ok(Self {
            sender_name: text(&d, "SenderComputerName").filter(|s| !s.is_empty()),
            sender_model: text(&d, "SenderModelName").unwrap_or_default(),
            sender_id: text(&d, "SenderID").unwrap_or_default(),
            files,
            urls,
            transfer_id: d.get("TransferID").and_then(Value::as_dictionary).and_then(|t| text(t, "id")),
        })
    }

    pub fn sender_label(&self) -> &str {
        self.sender_name.as_deref().unwrap_or("Unknown device")
    }
}
