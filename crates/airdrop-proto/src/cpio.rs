//! The archive a sender streams in its /Upload body: odc ("070707") cpio entries inside
//! Apple's dvzip container, produced on the fly without buffering whole files.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAGIC_ODC: &[u8; 6] = b"070707";
pub const MAGIC_NEWC: &[u8; 6] = b"070701";
pub const MAGIC_NEWC_CRC: &[u8; 6] = b"070702";
pub const TRAILER: &[u8] = b"TRAILER!!!";
pub const ODC_HEADER: usize = 76;
pub const NEWC_HEADER: usize = 110;
/// odc stores sizes in 11 octal digits: just under 8 GiB.
pub const MAX_ODC_SIZE: u64 = 8u64.pow(11) - 1;
pub const DVZIP_BLOCK: usize = 128 * 1024;
pub const DVZIP_STORED: u32 = 0x8000_0000;
const READ_SIZE: usize = 1 << 20;
/// What a Mac sender uses; ownership is ignored by receivers.
const UID: u32 = 501;
const GID: u32 = 20;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

#[derive(Debug, Clone)]
pub struct Entry {
    pub arcname: Vec<u8>,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
    /// Permission bits only (0755 / 0644).
    pub mode: u32,
}

#[derive(Debug, Clone)]
pub struct ArchivePlan {
    pub entries: Vec<Entry>,
    /// Bytes of file data (what the progress line counts and /Upload TotalBytes says).
    pub total_bytes: u64,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn mtime_of(m: &fs::Metadata) -> u64 {
    u64::try_from(m.mtime()).unwrap_or(0)
}

/// Stat everything up front: total size for the progress line and /Ask, and early errors.
/// Symlinks are followed (a dangling one is skipped, a folder loop is an error); sockets,
/// FIFOs and devices are skipped.
pub fn plan_archive(paths: &[PathBuf]) -> io::Result<ArchivePlan> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // iOS 27.2 starts every upload with the archive root "." (040755), measured from photo
    // and contact transfers; receivers that mirror iOS may expect it.
    let mut plan = ArchivePlan {
        entries: vec![Entry { arcname: b".".to_vec(), path: PathBuf::from("."), is_dir: true, size: 0, mtime: now, mode: 0o755 }],
        total_bytes: 0,
    };
    let mut seen = std::collections::HashSet::new();
    for p in paths {
        let name = p.file_name().ok_or_else(|| invalid(format!("{}: cannot send a path without a name", p.display())))?;
        if !seen.insert(name.to_owned()) {
            return Err(invalid(format!("two items share the name {:?}; rename one before sending", name)));
        }
        let mut arc = b"./".to_vec();
        arc.extend_from_slice(name.as_bytes());
        add(&mut plan, p, arc, &mut vec![])?;
    }
    Ok(plan)
}

/// `stack` holds the (device, inode) of the folders we are inside, to catch symlink loops.
fn add(plan: &mut ArchivePlan, path: &Path, arc: Vec<u8>, stack: &mut Vec<(u64, u64)>) -> io::Result<()> {
    let mut meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        match fs::metadata(path) {
            Ok(m) => meta = m,
            Err(_) => return Ok(()), // dangling link: skip
        }
    }
    let ft = meta.file_type();
    if ft.is_dir() {
        let id = (meta.dev(), meta.ino());
        if stack.contains(&id) {
            return Err(invalid(format!("{} links back into a folder being sent", path.display())));
        }
        plan.entries.push(Entry { arcname: arc.clone(), path: path.into(), is_dir: true, size: 0, mtime: mtime_of(&meta), mode: 0o755 });
        let mut children: Vec<_> = fs::read_dir(path)?.collect::<io::Result<_>>()?;
        children.sort_by_key(|e| e.file_name());
        stack.push(id);
        for child in children {
            let mut child_arc = arc.clone();
            child_arc.push(b'/');
            child_arc.extend_from_slice(child.file_name().as_bytes());
            add(plan, &child.path(), child_arc, stack)?;
        }
        stack.pop();
    } else if ft.is_file() {
        if meta.len() > MAX_ODC_SIZE {
            return Err(invalid(format!("{} is larger than 8 GiB, which AirDrop's cpio format cannot carry", path.display())));
        }
        let mode = if meta.mode() & 0o111 != 0 { 0o755 } else { 0o644 };
        plan.entries.push(Entry { arcname: arc, path: path.into(), is_dir: false, size: meta.len(), mtime: mtime_of(&meta), mode });
        plan.total_bytes += meta.len();
    } else {
        debug_assert!(ft.is_socket() || ft.is_fifo() || ft.is_block_device() || ft.is_char_device());
    }
    Ok(())
}

impl ArchivePlan {
    /// Bytes per top-level item (a folder counts its files), for the /Ask `FileSize`.
    pub fn item_size(&self, name: &[u8]) -> u64 {
        self.entries.iter()
            .filter(|e| !e.is_dir && e.arcname.split(|&b| b == b'/').nth(1) == Some(name))
            .map(|e| e.size)
            .sum()
    }
}

pub fn odc_header(name: &[u8], mode: u32, size: u64, mtime: u64, ino: u64, nlink: u32) -> Vec<u8> {
    let mut h = format!(
        "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
        0, ino & 0o777777, mode, UID, GID, nlink, 0, mtime & 0o777_7777_7777, name.len() + 1, size,
    ).into_bytes();
    h.extend_from_slice(name);
    h.push(0);
    h
}

enum Part {
    /// Header bytes not yet handed out, from `pos`.
    Header { bytes: Vec<u8>, pos: usize },
    /// File data still to read.
    Data { file: File, left: u64, path: PathBuf },
    /// Before the next entry (or the trailer).
    Between,
    Done,
}

/// The raw cpio stream of a plan, as a `Read`. `on_data` sees each file-data read.
pub struct ArchiveReader {
    entries: std::vec::IntoIter<Entry>,
    ino: u64,
    part: Part,
    /// The file whose header is going out now; its data follows the header.
    queued: Option<(File, u64, PathBuf)>,
    trailer_sent: bool,
    on_data: Box<dyn FnMut(u64) + Send>,
}

impl ArchiveReader {
    pub fn new(plan: ArchivePlan, on_data: Box<dyn FnMut(u64) + Send>) -> Self {
        Self { entries: plan.entries.into_iter(), ino: 0, part: Part::Between, queued: None, trailer_sent: false, on_data }
    }

    fn advance(&mut self) -> io::Result<()> {
        if let Some((file, left, path)) = self.queued.take() {
            self.part = if left > 0 { Part::Data { file, left, path } } else { Part::Between };
            return Ok(());
        }
        self.part = match self.entries.next() {
            Some(e) => {
                self.ino += 1;
                if e.is_dir {
                    Part::Header { bytes: odc_header(&e.arcname, S_IFDIR | e.mode, 0, e.mtime, self.ino, 2), pos: 0 }
                } else {
                    let file = File::open(&e.path)?;
                    // SAFETY: posix_fadvise on an fd we own; a failure only loses a hint.
                    unsafe { libc::posix_fadvise(std::os::fd::AsRawFd::as_raw_fd(&file), 0, 0, libc::POSIX_FADV_SEQUENTIAL) };
                    let header = odc_header(&e.arcname, S_IFREG | e.mode, e.size, e.mtime, self.ino, 1);
                    self.queued = Some((file, e.size, e.path));
                    Part::Header { bytes: header, pos: 0 }
                }
            }
            None if !self.trailer_sent => {
                self.trailer_sent = true;
                Part::Header { bytes: odc_header(TRAILER, 0, 0, 0, 0, 1), pos: 0 }
            }
            None => Part::Done,
        };
        Ok(())
    }
}

impl Read for ArchiveReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match &mut self.part {
                Part::Done => return Ok(0),
                Part::Between => self.advance()?,
                Part::Header { bytes, pos } => {
                    let n = (bytes.len() - *pos).min(buf.len());
                    buf[..n].copy_from_slice(&bytes[*pos..*pos + n]);
                    *pos += n;
                    if *pos == bytes.len() {
                        self.part = Part::Between;
                    }
                    return Ok(n);
                }
                Part::Data { file, left, path } => {
                    let want = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                    let n = file.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(io::Error::other(format!("{} shrank while being sent", path.display())));
                    }
                    *left -= n as u64;
                    if *left == 0 {
                        self.part = Part::Between;
                    }
                    (self.on_data)(n as u64);
                    return Ok(n);
                }
            }
        }
    }
}

/// Apple's dvzip container, stored blocks only (what iOS 26+ sends and expects on
/// /Upload): per 128 KiB of cpio a 4-byte big-endian header (bit 31 = stored, low 31 bits
/// = length) and the bytes, no trailer. Stored keeps the length known up front while
/// streaming; AirDrop payloads are mostly already-compressed media.
pub struct DvzipBlocks<R> {
    inner: R,
    done: bool,
}

impl<R: Read> DvzipBlocks<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, done: false }
    }
}

impl<R: Read> Iterator for DvzipBlocks<R> {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut block = vec![0u8; 4 + DVZIP_BLOCK];
        let mut filled = 4;
        while filled < block.len() {
            match self.inner.read(&mut block[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
        let len = filled - 4;
        if len < DVZIP_BLOCK {
            self.done = true;
            if len == 0 {
                return None;
            }
        }
        block.truncate(filled);
        block[..4].copy_from_slice(&(DVZIP_STORED | len as u32).to_be_bytes());
        Some(Ok(block))
    }
}

/// Read size used by the extractor and sender pipelines.
pub const CHUNK: usize = READ_SIZE;
