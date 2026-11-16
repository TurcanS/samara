//! A minimal generic-netlink client: just enough for nl80211 (requests, acks,
//! dumps, multicast events). Hand-rolled so every attribute on the wire is visible.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_DUMP: u16 = 0x300;
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const CTRL_ATTR_MCAST_GROUPS: u16 = 7;
const CTRL_ATTR_MCAST_GRP_NAME: u16 = 1;
const CTRL_ATTR_MCAST_GRP_ID: u16 = 2;
const SOL_NETLINK: libc::c_int = 270;
const NETLINK_ADD_MEMBERSHIP: libc::c_int = 1;
const NLA_F_NESTED: u16 = 0x8000;
const HDRS: usize = 16 + 4;

/// Attributes under construction.
#[derive(Default, Clone)]
pub struct Attrs(Vec<u8>);

impl Attrs {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn bytes(mut self, ty: u16, data: &[u8]) -> Self {
        let len = 4 + data.len();
        self.0.extend_from_slice(&(len as u16).to_ne_bytes());
        self.0.extend_from_slice(&ty.to_ne_bytes());
        self.0.extend_from_slice(data);
        self.0.resize(self.0.len().next_multiple_of(4), 0);
        self
    }
    pub fn u32(self, ty: u16, v: u32) -> Self {
        self.bytes(ty, &v.to_ne_bytes())
    }
    pub fn u64(self, ty: u16, v: u64) -> Self {
        self.bytes(ty, &v.to_ne_bytes())
    }
    pub fn flag(self, ty: u16) -> Self {
        self.bytes(ty, &[])
    }
    pub fn string(self, ty: u16, s: &str) -> Self {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        self.bytes(ty, &v)
    }
    pub fn nested(self, ty: u16, inner: Attrs) -> Self {
        self.bytes(ty | NLA_F_NESTED, &inner.0)
    }
}

/// One received attribute (borrowed from the message buffer).
#[derive(Clone, Copy)]
pub struct Attr<'a> {
    pub ty: u16,
    pub data: &'a [u8],
}

impl Attr<'_> {
    pub fn u32(&self) -> Option<u32> {
        Some(u32::from_ne_bytes(self.data.get(..4)?.try_into().ok()?))
    }
    pub fn u64(&self) -> Option<u64> {
        Some(u64::from_ne_bytes(self.data.get(..8)?.try_into().ok()?))
    }
    pub fn str(&self) -> Option<String> {
        let end = self.data.iter().position(|&b| b == 0).unwrap_or(self.data.len());
        Some(String::from_utf8_lossy(&self.data[..end]).into_owned())
    }
    pub fn children(&self) -> Vec<Attr<'_>> {
        parse_attrs(self.data)
    }
}

pub fn parse_attrs(mut buf: &[u8]) -> Vec<Attr<'_>> {
    let mut out = Vec::new();
    while buf.len() >= 4 {
        let len = u16::from_ne_bytes([buf[0], buf[1]]) as usize;
        let ty = u16::from_ne_bytes([buf[2], buf[3]]) & !NLA_F_NESTED & !0x4000;
        if len < 4 || len > buf.len() {
            break;
        }
        out.push(Attr { ty, data: &buf[4..len] });
        buf = &buf[len.next_multiple_of(4).min(buf.len())..];
    }
    out
}

pub fn find<'a>(attrs: &[Attr<'a>], ty: u16) -> Option<Attr<'a>> {
    attrs.iter().copied().find(|a| a.ty == ty)
}

/// A received generic-netlink message.
pub struct Msg {
    pub cmd: u8,
    payload: Vec<u8>,
}

impl Msg {
    pub fn attrs(&self) -> Vec<Attr<'_>> {
        parse_attrs(&self.payload)
    }
}

pub struct Socket {
    fd: OwnedFd,
    seq: u32,
    pub family: u16,
}

impl Socket {
    /// Open a socket and resolve `family` ("nl80211").
    pub fn open(family: &str) -> io::Result<Self> {
        let mut s = Self::raw()?;
        let reply = s.request_raw(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 1, NLM_F_REQUEST | NLM_F_ACK,
            &Attrs::new().string(CTRL_ATTR_FAMILY_NAME, family))?;
        let msg = reply.into_iter().next().ok_or_else(|| io::Error::other("no family reply"))?;
        s.family = find(&msg.attrs(), CTRL_ATTR_FAMILY_ID)
            .and_then(|a| a.data.get(..2).map(|b| u16::from_ne_bytes([b[0], b[1]])))
            .ok_or_else(|| io::Error::other(format!("netlink family {family} not available")))?;
        Ok(s)
    }

    fn raw() -> io::Result<Self> {
        // SAFETY: plain socket(2); the fd is owned immediately.
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_GENERIC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is valid and exclusively ours.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: sockaddr_nl is plain old data; all-zero is valid.
        let mut sa: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        sa.nl_family = libc::AF_NETLINK as u16;
        // SAFETY: sa is a valid sockaddr_nl of the stated size.
        let rc = unsafe { libc::bind(fd.as_raw_fd(), std::ptr::from_ref(&sa).cast(), size_of::<libc::sockaddr_nl>() as u32) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut s = Self { fd, seq: 1, family: GENL_ID_CTRL };
        s.set_timeout(Duration::from_secs(5))?;
        Ok(s)
    }

    pub fn set_timeout(&mut self, d: Duration) -> io::Result<()> {
        let tv = libc::timeval { tv_sec: d.as_secs() as _, tv_usec: d.subsec_micros() as _ };
        // SAFETY: tv outlives the call and the length matches.
        let rc = unsafe {
            libc::setsockopt(self.fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVTIMEO,
                std::ptr::from_ref(&tv).cast(), size_of::<libc::timeval>() as u32)
        };
        if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Id of a multicast group of the resolved family, e.g. ("nl80211", "scan").
    pub fn group_id(&mut self, family: &str, group: &str) -> io::Result<u32> {
        let reply = self.request_raw(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 1, NLM_F_REQUEST | NLM_F_ACK,
            &Attrs::new().string(CTRL_ATTR_FAMILY_NAME, family))?;
        let msg = reply.first().ok_or_else(|| io::Error::other("no family reply"))?;
        let attrs = msg.attrs();
        let groups = find(&attrs, CTRL_ATTR_MCAST_GROUPS).ok_or_else(|| io::Error::other("no multicast groups"))?;
        for g in groups.children() {
            let inner = g.children();
            let name = find(&inner, CTRL_ATTR_MCAST_GRP_NAME).and_then(|a| a.str());
            if name.as_deref() == Some(group)
                && let Some(id) = find(&inner, CTRL_ATTR_MCAST_GRP_ID).and_then(|a| a.u32()) {
                return Ok(id);
            }
        }
        Err(io::Error::other(format!("multicast group {group} not found")))
    }

    pub fn join(&self, group: u32) -> io::Result<()> {
        // SAFETY: group outlives the call and the length matches.
        let rc = unsafe {
            libc::setsockopt(self.fd.as_raw_fd(), SOL_NETLINK, NETLINK_ADD_MEMBERSHIP,
                std::ptr::from_ref(&group).cast(), size_of::<u32>() as u32)
        };
        if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Command with an ACK; returns the reply messages (usually none).
    pub fn request(&mut self, cmd: u8, attrs: &Attrs) -> io::Result<Vec<Msg>> {
        self.request_raw(self.family, cmd, 0, NLM_F_REQUEST | NLM_F_ACK, attrs)
    }

    /// A dump: every message the kernel sends until NLMSG_DONE.
    pub fn dump(&mut self, cmd: u8, attrs: &Attrs) -> io::Result<Vec<Msg>> {
        self.request_raw(self.family, cmd, 0, NLM_F_REQUEST | NLM_F_DUMP, attrs)
    }

    fn request_raw(&mut self, ty: u16, cmd: u8, version: u8, flags: u16, attrs: &Attrs) -> io::Result<Vec<Msg>> {
        self.seq += 1;
        let seq = self.seq;
        let len = HDRS + attrs.0.len();
        let mut buf = Vec::with_capacity(len);
        buf.extend_from_slice(&(len as u32).to_ne_bytes());
        buf.extend_from_slice(&ty.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(&[cmd, version, 0, 0]);
        buf.extend_from_slice(&attrs.0);
        // SAFETY: buf is a valid readable buffer of len bytes.
        if unsafe { libc::send(self.fd.as_raw_fd(), buf.as_ptr().cast(), buf.len(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut out = Vec::new();
        loop {
            let batch = match self.recv_batch() {
                Ok(b) => b,
                // SIGTERM only sets a flag; the request itself must still complete.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            // Replies to an earlier request that timed out are not ours.
            for raw in batch.into_iter().filter(|r| r.seq == seq) {
                match raw.ty {
                    NLMSG_ERROR => {
                        let code = raw.payload.get(..4).map_or(0, |b| i32::from_ne_bytes([b[0], b[1], b[2], b[3]]));
                        return if code == 0 { Ok(out) } else { Err(io::Error::from_raw_os_error(-code)) };
                    }
                    NLMSG_DONE => return Ok(out),
                    _ => out.push(Self::to_msg(&raw.payload)),
                }
            }
        }
    }

    fn to_msg(payload: &[u8]) -> Msg {
        Msg { cmd: payload.first().copied().unwrap_or(0), payload: payload.get(4..).unwrap_or(&[]).to_vec() }
    }

    /// Next datagram from the socket, split into its netlink messages.
    fn recv_batch(&mut self) -> io::Result<Vec<Raw>> {
        let mut buf = vec![0u8; 65536];
        // SAFETY: buf is valid and writable for its full length.
        let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut data = &buf[..n as usize];
        let mut out = Vec::new();
        while data.len() >= 16 {
            let len = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]) as usize;
            if len < 16 || len > data.len() {
                break;
            }
            out.push(Raw {
                ty: u16::from_ne_bytes([data[4], data[5]]),
                seq: u32::from_ne_bytes([data[8], data[9], data[10], data[11]]),
                payload: data[16..len].to_vec(),
            });
            data = &data[len.next_multiple_of(4).min(data.len())..];
        }
        Ok(out)
    }

    /// Multicast messages that arrived; `Ok(None)` on timeout. Errors (ENOBUFS when
    /// events overflowed) are the caller's to survive.
    pub fn next_event(&mut self) -> io::Result<Option<Vec<Msg>>> {
        match self.recv_batch() {
            Ok(batch) => Ok(Some(batch.iter().filter(|r| r.ty == self.family).map(|r| Self::to_msg(&r.payload)).collect())),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// One netlink message as received: type, sequence number, payload after nlmsghdr.
struct Raw {
    ty: u16,
    seq: u32,
    payload: Vec<u8>,
}
