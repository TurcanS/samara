//! The receiving side of an /Upload: undo the container (dvzip, gzip or raw), parse the
//! cpio stream (odc or newc) as it arrives, and write files under the download folder.
//!
//! Safety rules, in the order they apply to each entry:
//! 1. an absolute name or a `..` component rejects the whole transfer;
//! 2. the /Ask `FileBomPath`s cut away the sender's folders above each announced item;
//! 3. leading `NSIRD_*` / `PHMediaFormatConversionRequest-*` staging folders are dropped;
//! 4. `._*` AppleDouble files, `__MACOSX`, symlinks, devices and the root `.` are skipped;
//! 5. what is left must start with a `FileName` from the /Ask (owly UP-7), else the whole
//!    transfer is rejected: what gets written is what the prompt named;
//! 6. files are written as `.<name>.airdrop-part` (`O_EXCL|O_NOFOLLOW`) and get their real
//!    name only when complete, by hard link (never overwriting: `name (1).ext`);
//! 7. `abort()` (or dropping an unfinished extractor) removes everything this transfer
//!    created, including items already published.

use crate::cpio::{DVZIP_BLOCK, DVZIP_STORED, MAGIC_NEWC, MAGIC_NEWC_CRC, MAGIC_ODC, NEWC_HEADER, ODC_HEADER, TRAILER};
use flate2::{Decompress, FlushDecompress, Status};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const MAX_DVZIP_BLOCK: usize = 8 << 20;
const MAX_NAME: usize = 4096;
const MAX_MEMBERS: usize = 10_000;
/// Input slice fed to gzip at a time: bounds what one call can inflate to (~1000:1).
const GZIP_SLICE: usize = 16 * 1024;

#[derive(Debug)]
pub enum ExtractError {
    /// The sender's archive is malformed or tries something unsafe (HTTP 400).
    Unsafe(String),
    /// Our side failed to write (HTTP 500).
    Io(io::Error),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsafe(m) => f.write_str(m),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ExtractError {}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

type Result<T> = std::result::Result<T, ExtractError>;

fn unsafe_archive(msg: impl Into<String>) -> ExtractError {
    ExtractError::Unsafe(msg.into())
}

// ---- container -------------------------------------------------------------------

/// Collects decoded output for the cpio parser.
#[derive(Default)]
struct Out(Vec<u8>);

impl Write for Out {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Container {
    /// Not yet known: the first two bytes decide (gzip magic or raw cpio).
    Sniffing(Vec<u8>),
    Raw,
    Gzip(Box<flate2::write::MultiGzDecoder<Out>>),
    Dvzip { header: Vec<u8>, need: usize, stored: bool, block: Vec<u8> },
}

impl Container {
    fn new(dvzip: bool) -> Self {
        if dvzip {
            Self::Dvzip { header: Vec::with_capacity(4), need: 0, stored: false, block: Vec::new() }
        } else {
            Self::Sniffing(Vec::new())
        }
    }

    /// Decode `data`, handing every piece of cpio stream to `sink`.
    fn feed(&mut self, mut data: &[u8], sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        if let Self::Sniffing(head) = self {
            head.extend_from_slice(data);
            if head.len() < 2 {
                return Ok(());
            }
            let head = std::mem::take(head);
            *self = if head[..2] == [0x1f, 0x8b] {
                Self::Gzip(Box::new(flate2::write::MultiGzDecoder::new(Out::default())))
            } else {
                Self::Raw
            };
            return self.feed(&head, sink);
        }
        match self {
            Self::Sniffing(_) => unreachable!("handled above"),
            Self::Raw => sink(data),
            Self::Gzip(d) => {
                for piece in data.chunks(GZIP_SLICE) {
                    d.write_all(piece).map_err(|e| unsafe_archive(format!("bad gzip stream: {e}")))?;
                    let out = std::mem::take(&mut d.get_mut().0);
                    if !out.is_empty() {
                        sink(&out)?;
                    }
                }
                Ok(())
            }
            Self::Dvzip { header, need, stored, block } => {
                while !data.is_empty() {
                    if *need == 0 {
                        let take = (4 - header.len()).min(data.len());
                        header.extend_from_slice(&data[..take]);
                        data = &data[take..];
                        if header.len() < 4 {
                            return Ok(());
                        }
                        let raw = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
                        header.clear();
                        *stored = raw & DVZIP_STORED != 0;
                        *need = (raw & 0x7FFF_FFFF) as usize;
                        if !(1..=MAX_DVZIP_BLOCK).contains(need) {
                            return Err(unsafe_archive("bad dvzip block length"));
                        }
                        continue;
                    }
                    let take = (*need).min(data.len());
                    if *stored {
                        sink(&data[..take])?; // stored: straight through
                    } else {
                        block.extend_from_slice(&data[..take]);
                    }
                    data = &data[take..];
                    *need -= take;
                    if *need == 0 && !*stored {
                        // zlib-wrapped deflate, at most one block of cpio
                        let mut d = Decompress::new(true);
                        let mut out = Vec::with_capacity(DVZIP_BLOCK + 1);
                        let status = d.decompress_vec(block, &mut out, FlushDecompress::Finish);
                        block.clear();
                        if !matches!(status, Ok(Status::StreamEnd)) || out.len() > DVZIP_BLOCK {
                            return Err(unsafe_archive("bad dvzip block"));
                        }
                        sink(&out)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        match self {
            Self::Sniffing(head) if !head.is_empty() => Err(unsafe_archive("truncated upload")),
            Self::Dvzip { header, need, .. } if *need > 0 || !header.is_empty() => Err(unsafe_archive("truncated dvzip stream")),
            Self::Gzip(d) => d.try_finish().map_err(|_| unsafe_archive("truncated gzip stream")),
            _ => Ok(()),
        }
    }
}

// ---- names -----------------------------------------------------------------------

/// Python's `os.path.splitext`: ("a.tar", ".gz"), (".bashrc", "").
fn split_ext(name: &[u8]) -> (&[u8], &[u8]) {
    let lead = name.iter().take_while(|&&b| b == b'.').count();
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i >= lead => name.split_at(i),
        _ => (name, &[]),
    }
}

fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Names tried per item before giving up (`name (1).ext` ... `name (10000).ext`).
const MAX_RENAMES: u32 = 10_000;

/// `parent/name`, or `parent/stem (n).ext` with the first free n.
fn unique(parent: &Path, name: &[u8]) -> Result<PathBuf> {
    let candidate = parent.join(OsStr::from_bytes(name));
    if !lexists(&candidate) {
        return Ok(candidate);
    }
    let (stem, ext) = split_ext(name);
    (1..=MAX_RENAMES)
        .map(|i| {
            let mut n = stem.to_vec();
            n.extend_from_slice(format!(" ({i})").as_bytes());
            n.extend_from_slice(ext);
            parent.join(OsStr::from_bytes(&n))
        })
        .find(|c| !lexists(c))
        .ok_or_else(|| ExtractError::Io(io::Error::other(format!("no free name for {} in {}", candidate.display(), parent.display()))))
}

fn components(path: &[u8]) -> Vec<Vec<u8>> {
    path.split(|&b| b == b'/').filter(|p| !p.is_empty() && *p != b".").map(<[u8]>::to_vec).collect()
}

/// Strict unsigned number: only digits of `radix` (no sign, space or `_`). A negative size
/// once made the parser walk backwards forever.
fn number(field: &[u8], radix: u32) -> Result<u64> {
    if field.is_empty() || !field.iter().all(|&b| (b as char).is_digit(radix)) {
        return Err(unsafe_archive("corrupt cpio header"));
    }
    let text = std::str::from_utf8(field).map_err(|_| unsafe_archive("corrupt cpio header"))?;
    u64::from_str_radix(text, radix).map_err(|_| unsafe_archive("corrupt cpio header"))
}

// ---- cpio ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Odc,
    Newc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Magic,
    Head(Format),
    Name,
    Data,
    Pad,
    Done,
}

/// One archive member as it arrived, for `--dump`.
#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    pub mode: u32,
    pub size: u64,
}

/// A file being written under its partial name.
struct Pending {
    file: File,
    /// Index of the partial name in `created_files`.
    slot: usize,
    parent: PathBuf,
    name: Vec<u8>,
    top_level: bool,
}

pub struct Extractor {
    dest: PathBuf,
    roots: Vec<Vec<Vec<u8>>>,
    /// The /Ask's `FileName`s: the only names allowed at the top of the download folder.
    names: HashSet<Vec<u8>>,
    container: Container,
    state: State,
    need: usize,
    buf: Vec<u8>,
    format: Format,
    mode: u32,
    mtime: u64,
    namesize: usize,
    /// Data bytes of the current entry still to come.
    left: u64,
    size: u64,
    name_pad: usize,
    /// A directory entry whose mtime to set at its end.
    dir: Option<PathBuf>,
    pending: Option<Pending>,
    top: HashMap<Vec<u8>, PathBuf>,
    created_files: Vec<PathBuf>,
    created_dirs: Vec<PathBuf>,
    saved: Vec<PathBuf>,
    members: Vec<Member>,
    bytes_written: u64,
    /// `finish()` succeeded: dropping must not remove anything.
    kept: bool,
}

impl Extractor {
    /// `dvzip`: the /Upload Content-Type said so; otherwise gzip or raw is detected.
    /// `roots`: the `FileBomPath` of every item in the accepted /Ask; `names`: their `FileName`s.
    pub fn new(dest: &Path, dvzip: bool, roots: &[String], names: &[String]) -> Self {
        Self {
            dest: dest.to_path_buf(),
            roots: roots.iter().map(|r| components(r.as_bytes())).filter(|r| !r.is_empty()).collect(),
            names: names.iter().map(|n| n.as_bytes().to_vec()).collect(),
            container: Container::new(dvzip),
            state: State::Magic,
            need: 6,
            buf: Vec::new(),
            format: Format::Odc,
            mode: 0,
            mtime: 0,
            namesize: 0,
            left: 0,
            size: 0,
            name_pad: 0,
            dir: None,
            pending: None,
            top: HashMap::new(),
            created_files: vec![],
            created_dirs: vec![],
            saved: vec![],
            members: vec![],
            bytes_written: 0,
            kept: false,
        }
    }

    /// The trailer arrived: every file is complete and published.
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Top-level items, in arrival order.
    pub fn saved(&self) -> &[PathBuf] {
        &self.saved
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<()> {
        if self.is_done() {
            return Ok(()); // padding after the trailer
        }
        let mut container = std::mem::replace(&mut self.container, Container::Raw);
        let result = container.feed(data, &mut |cpio| self.consume(cpio));
        self.container = container;
        result
    }

    /// The body ended: the archive must be complete. Keeps the files.
    pub fn finish(&mut self) -> Result<Vec<PathBuf>> {
        if !self.is_done() {
            self.container.finish()?;
            if !self.is_done() {
                return Err(unsafe_archive("archive ended before its trailer"));
            }
        }
        self.kept = true;
        Ok(self.saved.clone())
    }

    /// Remove everything this transfer created (files, then folders, newest first).
    pub fn abort(&mut self) {
        self.pending = None;
        for p in self.created_files.drain(..).rev() {
            let _ = fs::remove_file(p);
        }
        for d in self.created_dirs.drain(..).rev() {
            let _ = fs::remove_dir(d);
        }
        self.saved.clear();
    }

    fn consume(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            match self.state {
                State::Done => return Ok(()),
                State::Data => {
                    let n = data.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
                    if let Some(p) = &mut self.pending {
                        p.file.write_all(&data[..n])?;
                        self.bytes_written += n as u64;
                    }
                    self.left -= n as u64;
                    data = &data[n..];
                    if self.left == 0 {
                        self.end_entry()?;
                    }
                }
                _ => {
                    let take = (self.need - self.buf.len()).min(data.len());
                    self.buf.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if self.buf.len() == self.need {
                        let buf = std::mem::take(&mut self.buf);
                        self.advance(&buf)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn advance(&mut self, buf: &[u8]) -> Result<()> {
        match self.state {
            State::Magic => {
                let format = match buf {
                    b if b == MAGIC_ODC => Format::Odc,
                    b if b == MAGIC_NEWC || b == MAGIC_NEWC_CRC => Format::Newc,
                    _ => return Err(unsafe_archive(format!("not a cpio archive (magic {:?})", String::from_utf8_lossy(buf)))),
                };
                self.format = format;
                self.state = State::Head(format);
                self.need = if format == Format::Odc { ODC_HEADER } else { NEWC_HEADER } - 6;
            }
            State::Head(Format::Odc) => {
                self.mode = number(&buf[12..18], 8)? as u32;
                self.mtime = number(&buf[42..53], 8)?;
                self.namesize = number(&buf[53..59], 8)? as usize;
                self.size = number(&buf[59..70], 8)?;
                self.name_pad = 0;
                self.after_header()?;
            }
            State::Head(Format::Newc) => {
                let field = |i: usize| number(&buf[i * 8..i * 8 + 8], 16);
                self.mode = field(1)? as u32;
                self.mtime = field(5)?;
                self.size = field(6)?;
                self.namesize = field(11)? as usize;
                self.name_pad = (4 - (NEWC_HEADER + self.namesize) % 4) % 4;
                self.after_header()?;
            }
            State::Name => {
                let raw = &buf[..self.namesize];
                let end = raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
                let name = &raw[..end];
                if self.members.len() < MAX_MEMBERS {
                    self.members.push(Member { name: String::from_utf8_lossy(name).into_owned(), mode: self.mode, size: self.size });
                }
                if name == TRAILER {
                    self.state = State::Done;
                    return Ok(());
                }
                let name = name.to_vec();
                self.left = self.size;
                self.begin_entry(&name)?;
                if self.size > 0 {
                    self.state = State::Data;
                } else {
                    self.end_entry()?;
                }
            }
            State::Pad => self.next_header(),
            State::Data | State::Done => unreachable!("not reached through the header buffer"),
        }
        Ok(())
    }

    fn after_header(&mut self) -> Result<()> {
        if !(1..=MAX_NAME).contains(&self.namesize) {
            return Err(unsafe_archive("bad name length"));
        }
        self.state = State::Name;
        self.need = self.namesize + self.name_pad;
        Ok(())
    }

    fn next_header(&mut self) {
        self.state = State::Magic;
        self.need = 6;
    }

    fn end_entry(&mut self) -> Result<()> {
        let mtime = (self.mtime > 0).then(|| UNIX_EPOCH + Duration::from_secs(self.mtime));
        if let Some(p) = self.pending.take() {
            if let Some(t) = mtime {
                let _ = p.file.set_modified(t);
            }
            drop(p.file);
            self.publish(p.slot, &p.parent, &p.name, p.top_level)?;
        }
        if let (Some(dir), Some(t)) = (self.dir.take(), mtime) {
            let _ = File::open(&dir).and_then(|f| f.set_modified(t));
        }
        let pad = if self.format == Format::Newc { ((4 - self.size % 4) % 4) as usize } else { 0 };
        if pad > 0 {
            self.state = State::Pad;
            self.need = pad;
        } else {
            self.next_header();
        }
        Ok(())
    }

    fn begin_entry(&mut self, name: &[u8]) -> Result<()> {
        let shown = String::from_utf8_lossy(name);
        if name.starts_with(b"/") {
            return Err(unsafe_archive(format!("absolute path in archive: {shown:?}")));
        }
        let mut parts = components(name);
        if parts.iter().any(|p| p == b"..") {
            return Err(unsafe_archive(format!("path traversal in archive: {shown:?}")));
        }
        // Several items from different apps' staging areas arrive under the iPhone's own
        // folder tree (2026-11-12: "Containers/Data/PluginKitPlugin/<UUID>/tmp/NSIRD_x/" and
        // "Media/PhotoData/OutgoingTemp/PHMedia.../"). /Ask names each item's place: keep the
        // item and what is inside it, and none of the folders above it.
        for root in &self.roots {
            if parts.starts_with(root) {
                parts.drain(..root.len() - 1);
                break;
            }
            if root.starts_with(&parts) {
                parts.clear(); // a folder above an announced item
                break;
            }
        }
        // iOS wraps items in NSItemProvider staging folders, and Photos in conversion folders.
        let staging = |p: &[u8]| p.starts_with(b"NSIRD_") || p.starts_with(b"PHMediaFormatConversionRequest-");
        let lead = parts.iter().take_while(|p| staging(p)).count();
        parts.drain(..lead);
        let skip = parts.iter().any(|p| p.starts_with(b"._") || p == b"__MACOSX");
        let kind = self.mode & 0o170000;
        if skip || parts.is_empty() || (kind != 0o100000 && kind != 0o040000) {
            return Ok(()); // root entry, sidecar, symlink, device: data is read and dropped
        }
        if !self.names.contains(&parts[0]) {
            return Err(unsafe_archive(format!("archive entry {shown:?} is not an item the sender announced")));
        }
        let is_dir = kind == 0o040000;
        let top = parts[0].clone();
        if parts.len() == 1 && !is_dir {
            let dest = self.dest.clone();
            return self.open_file(&dest, &top, true);
        }
        let base = match self.top.get(&top) {
            Some(b) => b.clone(),
            None => {
                let b = unique(&self.dest, &top)?;
                self.make_dir(&b, true)?;
                self.top.insert(top, b.clone());
                b
            }
        };
        if is_dir {
            let target = self.make_parents(&base, &parts[1..])?;
            self.dir = Some(target);
            return Ok(());
        }
        let parent = self.make_parents(&base, &parts[1..parts.len() - 1])?;
        let file = parts.last().expect("at least two parts").clone();
        self.open_file(&parent, &file, false)
    }

    fn make_dir(&mut self, path: &Path, top_level: bool) -> Result<()> {
        fs::DirBuilder::new().mode(0o755).create(path)?;
        self.created_dirs.push(path.to_path_buf());
        if top_level {
            self.saved.push(path.to_path_buf());
        }
        Ok(())
    }

    fn make_parents(&mut self, base: &Path, parts: &[Vec<u8>]) -> Result<PathBuf> {
        let mut cur = base.to_path_buf();
        for p in parts {
            cur.push(OsStr::from_bytes(p));
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(unsafe_archive(format!("path conflict in archive at {}", cur.display()))),
                Err(_) => self.make_dir(&cur, false)?,
            }
        }
        Ok(cur)
    }

    fn open_file(&mut self, parent: &Path, name: &[u8], top_level: bool) -> Result<()> {
        // A hidden partial name until the last byte is written: a transfer that is cut off
        // never leaves a truncated file that looks complete.
        let mode = if self.mode & 0o111 != 0 { 0o755 } else { 0o644 };
        let mut base = OsString::from(".");
        base.push(OsStr::from_bytes(name));
        base.push(".airdrop-part");
        if base.len() > 240 {
            base = OsString::from(".airdrop-part");
        }
        for n in 0u32.. {
            let mut candidate = base.clone();
            if n > 0 {
                candidate.push(format!(".{n}"));
            }
            let part = parent.join(&candidate);
            match OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc::O_NOFOLLOW).open(&part) {
                Ok(file) => {
                    self.created_files.push(part);
                    let slot = self.created_files.len() - 1;
                    self.pending = Some(Pending { file, slot, parent: parent.to_path_buf(), name: name.to_vec(), top_level });
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        unreachable!("u32 range exhausted")
    }

    /// Give a finished partial file a free final name, never replacing anything.
    fn publish(&mut self, slot: usize, parent: &Path, name: &[u8], top_level: bool) -> Result<()> {
        let part = self.created_files[slot].clone();
        let fin = loop {
            let fin = unique(parent, name)?;
            match fs::hard_link(&part, &fin) {
                Ok(()) => {
                    fs::remove_file(&part)?;
                    break fin;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    // a filesystem without hard links
                    if lexists(&fin) {
                        continue;
                    }
                    fs::rename(&part, &fin)?;
                    break fin;
                }
            }
        };
        self.created_files[slot] = fin.clone();
        if top_level {
            self.saved.push(fin);
        }
        Ok(())
    }
}

/// Fail-safe: an extractor dropped before `finish()` succeeded (an error, a cancelled
/// task, a panic) removes what it wrote.
impl Drop for Extractor {
    fn drop(&mut self) {
        if !self.kept {
            self.abort();
        }
    }
}
