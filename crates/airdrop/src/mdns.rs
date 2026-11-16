//! mDNS for `_airdrop._tcp`, pinned to the AWDL interface over IPv6: a small responder
//! (our PTR/SRV/TXT/AAAA) and a browser, written on DNS bytes directly.
//!
//! The socket is bound to `[ff02::fb%awdl0]:5353`, which ties it to awdl0 without root.
//! No name probing: our instance name is our own random 12-hex-digit id, and the 1.5 s
//! probing costs is a third of the time an iPhone waits for `airdrop listen`.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const GROUP: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
const PORT: u16 = 5353;
const SERVICE: &str = "_airdrop._tcp.local";
const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_AAAA: u16 = 28;
const T_SRV: u16 = 33;
const T_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const CACHE_FLUSH: u16 = 0x8000;
/// TTLs python-zeroconf used: host records 120 s, the others 4500 s.
const HOST_TTL: u32 = 120;
const OTHER_TTL: u32 = 4500;

// ---- DNS bytes ----------------------------------------------------------------------

fn put_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let l = &label.as_bytes()[..label.len().min(63)];
        out.push(l.len() as u8);
        out.extend_from_slice(l);
    }
    out.push(0);
}

fn put_record(out: &mut Vec<u8>, name: &str, rtype: u16, unique: bool, ttl: u32, rdata: &[u8]) {
    put_name(out, name);
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&(CLASS_IN | if unique { CACHE_FLUSH } else { 0 }).to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

fn name_bytes(name: &str) -> Vec<u8> {
    let mut v = vec![];
    put_name(&mut v, name);
    v
}

/// A name at `pos`, following compression pointers; returns it and the offset after it.
fn read_name(msg: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels = vec![];
    let mut end = None;
    for _ in 0..64 {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(pos + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *msg.get(pos + 1)? as usize;
            end.get_or_insert(pos + 2);
            pos = ptr;
            continue;
        }
        labels.push(String::from_utf8_lossy(msg.get(pos + 1..pos + 1 + len)?).into_owned());
        pos += 1 + len;
    }
    None // a pointer loop
}

fn u16_at(m: &[u8], p: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*m.get(p)?, *m.get(p + 1)?]))
}

struct Record {
    name: String,
    ttl: u32,
    data: RData,
}

enum RData {
    Ptr(String),
    Srv { port: u16, target: String },
    Txt(Vec<(String, String)>),
    Aaaa(Ipv6Addr),
    Other,
}

struct Message {
    response: bool,
    questions: Vec<(String, u16)>,
    records: Vec<Record>,
}

fn parse(msg: &[u8]) -> Option<Message> {
    let flags = u16_at(msg, 2)?;
    let counts: Vec<usize> = (0..4).map(|i| u16_at(msg, 4 + 2 * i).map(usize::from)).collect::<Option<_>>()?;
    let mut pos = 12;
    let mut questions = vec![];
    for _ in 0..counts[0] {
        let (name, p) = read_name(msg, pos)?;
        questions.push((name.to_ascii_lowercase(), u16_at(msg, p)?));
        pos = p + 4;
    }
    let mut records = vec![];
    for _ in 0..counts[1] + counts[2] + counts[3] {
        let (name, p) = read_name(msg, pos)?;
        let rtype = u16_at(msg, p)?;
        let ttl = u32::from_be_bytes(msg.get(p + 4..p + 8)?.try_into().ok()?);
        let len = usize::from(u16_at(msg, p + 8)?);
        let start = p + 10;
        let rd = msg.get(start..start + len)?;
        let data = match rtype {
            T_PTR => RData::Ptr(read_name(msg, start)?.0),
            T_SRV => RData::Srv { port: u16_at(msg, start + 4)?, target: read_name(msg, start + 6)?.0 },
            T_AAAA if len == 16 => RData::Aaaa(Ipv6Addr::from(<[u8; 16]>::try_from(rd).ok()?)),
            T_TXT => {
                let mut items = vec![];
                let mut i = 0;
                while i < rd.len() {
                    let l = rd[i] as usize;
                    let s = String::from_utf8_lossy(rd.get(i + 1..i + 1 + l)?).into_owned();
                    let (k, v) = s.split_once('=').map_or((s.clone(), String::new()), |(k, v)| (k.to_string(), v.to_string()));
                    if !k.is_empty() {
                        items.push((k, v));
                    }
                    i += 1 + l;
                }
                RData::Txt(items)
            }
            _ => RData::Other,
        };
        records.push(Record { name: name.to_ascii_lowercase(), ttl, data });
        pos = start + len;
    }
    Some(Message { response: flags & 0x8000 != 0, questions, records })
}

// ---- state ---------------------------------------------------------------------------

/// What we publish.
struct Ours {
    instance: String,
    host: String,
    port: u16,
    address: Ipv6Addr,
    txt: String,
}

impl Ours {
    fn records(&self, out: &mut Vec<u8>, which: &[u16], ttl_scale: u32) -> u16 {
        let mut n = 0;
        for &t in which {
            match t {
                T_PTR => put_record(out, SERVICE, T_PTR, false, OTHER_TTL * ttl_scale, &name_bytes(&self.instance)),
                T_SRV => {
                    let mut rd = vec![0, 0, 0, 0];
                    rd.extend_from_slice(&self.port.to_be_bytes());
                    put_name(&mut rd, &self.host);
                    put_record(out, &self.instance, T_SRV, true, HOST_TTL * ttl_scale, &rd);
                }
                T_TXT => {
                    let mut rd = vec![self.txt.len() as u8];
                    rd.extend_from_slice(self.txt.as_bytes());
                    put_record(out, &self.instance, T_TXT, true, OTHER_TTL * ttl_scale, &rd);
                }
                T_AAAA => put_record(out, &self.host, T_AAAA, true, HOST_TTL * ttl_scale, &self.address.octets()),
                _ => continue,
            }
            n += 1;
        }
        n
    }

    /// A response packet with these answers (`ttl_scale` 0 = goodbye).
    fn packet(&self, answers: &[u16], additional: &[u16], ttl_scale: u32) -> Vec<u8> {
        let mut body = vec![];
        let an = self.records(&mut body, answers, ttl_scale);
        let ar = self.records(&mut body, additional, ttl_scale);
        let mut out = vec![0, 0, 0x84, 0, 0, 0];
        out.extend_from_slice(&an.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&ar.to_be_bytes());
        out.extend(body);
        out
    }
}

/// A resolved receiver.
#[derive(Debug, Clone)]
pub struct Found {
    pub name: String,
    pub addr: Ipv6Addr,
    pub port: u16,
    pub txt: Vec<(String, String)>,
}

#[derive(Default)]
struct Instance {
    srv: Option<(u16, String)>,
    txt: Vec<(String, String)>,
}

struct Browse {
    tx: mpsc::Sender<Found>,
    ignore: String,
    instances: HashMap<String, Instance>,
    hosts: HashMap<String, Vec<Ipv6Addr>>,
    reported: HashSet<String>,
}

impl Browse {
    fn learn(&mut self, records: &[Record]) {
        for r in records {
            match &r.data {
                RData::Ptr(target) if r.name == SERVICE && r.ttl > 0 => {
                    let t = target.to_ascii_lowercase();
                    if !t.starts_with(&self.ignore) {
                        self.instances.entry(t).or_default();
                    }
                }
                RData::Srv { port, target } => {
                    if let Some(i) = self.instances.get_mut(&r.name) {
                        i.srv = Some((*port, target.to_ascii_lowercase()));
                    } else if r.name.ends_with(SERVICE) && !r.name.starts_with(&self.ignore) {
                        self.instances.insert(r.name.clone(), Instance { srv: Some((*port, target.to_ascii_lowercase())), txt: vec![] });
                    }
                }
                RData::Txt(items) => {
                    if let Some(i) = self.instances.get_mut(&r.name) {
                        i.txt.clone_from(items);
                    }
                }
                RData::Aaaa(a) => {
                    let list = self.hosts.entry(r.name.clone()).or_default();
                    if !list.contains(a) {
                        list.push(*a);
                    }
                }
                _ => {}
            }
        }
        for (name, inst) in &self.instances {
            let Some((port, target)) = &inst.srv else { continue };
            let Some(addrs) = self.hosts.get(target) else { continue };
            if self.reported.contains(name) {
                continue;
            }
            // Only a link-local address: a receiver on awdl0 is reached there. Anything else
            // a record names would send our requests to some other host.
            let addr = addrs.iter().find(|a| a.segments()[0] & 0xffc0 == 0xfe80);
            if let Some(&addr) = addr {
                self.reported.insert(name.clone());
                let _ = self.tx.try_send(Found { name: name.clone(), addr, port: *port, txt: inst.txt.clone() });
            }
        }
    }

    /// Questions for what is still missing: the service, and each unresolved instance.
    fn questions(&self) -> Vec<(String, u16)> {
        let mut q = vec![(SERVICE.to_string(), T_PTR)];
        for (name, inst) in &self.instances {
            if self.reported.contains(name) {
                continue;
            }
            match &inst.srv {
                None => q.push((name.clone(), T_ANY)),
                Some((_, target)) => q.push((target.clone(), T_AAAA)),
            }
        }
        q
    }
}

#[derive(Default)]
struct State {
    ours: Option<Ours>,
    browse: Option<Browse>,
}

pub struct Mdns {
    sock: Arc<UdpSocket>,
    dest: SocketAddr,
    state: Arc<Mutex<State>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Mdns {
    pub fn open(iface: &str) -> io::Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};
        let name = std::ffi::CString::new(iface).map_err(io::Error::other)?;
        // SAFETY: if_nametoindex reads a NUL-terminated string.
        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if index == 0 {
            return Err(io::Error::other(format!("{iface}: no such interface")));
        }
        let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_reuse_address(true)?;
        s.set_reuse_port(true)?;
        s.set_only_v6(true)?;
        s.set_multicast_if_v6(index)?;
        s.set_multicast_hops_v6(255)?;
        s.set_multicast_loop_v6(false)?;
        s.join_multicast_v6(&GROUP, index)?;
        let dest = SocketAddr::V6(SocketAddrV6::new(GROUP, PORT, 0, index));
        s.bind(&dest.into())?; // the scope binds the socket to the interface
        s.set_nonblocking(true)?;
        let sock = Arc::new(UdpSocket::from_std(s.into())?);
        let state = Arc::new(Mutex::new(State::default()));
        let receiver = tokio::spawn(receive(sock.clone(), dest, state.clone()));
        Ok(Self { sock, dest, state, tasks: vec![receiver] })
    }

    /// Publish our receiver and keep re-announcing it (owly MDNS-7): almost continuously for
    /// the first 12 s (an iPhone that has not talked to us yet is on our channel in 2 of 16
    /// slots), then a round of three every 3 s for the whole session.
    pub async fn register(&mut self, service_id: &str, port: u16, address: Ipv6Addr, flags: u64) {
        let ours = Ours {
            instance: format!("{service_id}.{SERVICE}"),
            host: format!("{service_id}.local"),
            port,
            address,
            txt: format!("flags={flags}"),
        };
        let packet = ours.packet(&[T_PTR], &[T_SRV, T_TXT, T_AAAA], 1);
        self.state.lock().expect("not poisoned").ours = Some(ours);
        let _ = self.sock.send_to(&packet, self.dest).await;
        let (sock, dest) = (self.sock.clone(), self.dest);
        self.tasks.push(tokio::spawn(async move {
            let start = Instant::now();
            loop {
                if start.elapsed() < Duration::from_secs(12) {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = sock.send_to(&packet, dest).await;
                } else {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    for i in 0..3 {
                        if i > 0 {
                            tokio::time::sleep(Duration::from_millis(225)).await;
                        }
                        let _ = sock.send_to(&packet, dest).await;
                    }
                }
            }
        }));
    }

    /// Look for receivers; each resolved one arrives once on the returned channel.
    pub fn browse(&mut self, ignore_id: &str) -> mpsc::Receiver<Found> {
        let (tx, rx) = mpsc::channel(64);
        self.state.lock().expect("not poisoned").browse = Some(Browse {
            tx, ignore: format!("{}.", ignore_id.to_ascii_lowercase()), instances: HashMap::new(), hosts: HashMap::new(), reported: HashSet::new(),
        });
        let (sock, dest, state) = (self.sock.clone(), self.dest, self.state.clone());
        self.tasks.push(tokio::spawn(async move {
            for wait in [0u64, 1, 1, 2].into_iter().chain(std::iter::repeat(3)) {
                tokio::time::sleep(Duration::from_secs(wait)).await;
                let questions = state.lock().expect("not poisoned").browse.as_ref().map(Browse::questions).unwrap_or_default();
                let mut q = vec![0, 0, 0, 0];
                q.extend_from_slice(&(questions.len() as u16).to_be_bytes());
                q.extend_from_slice(&[0; 6]);
                for (name, t) in &questions {
                    put_name(&mut q, name);
                    q.extend_from_slice(&t.to_be_bytes());
                    q.extend_from_slice(&CLASS_IN.to_be_bytes());
                }
                let _ = sock.send_to(&q, dest).await;
            }
        }));
        rx
    }

    /// Stop announcing and say goodbye (our records with TTL 0).
    pub async fn close(mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        let goodbye = self.state.lock().expect("not poisoned").ours.as_ref().map(|o| o.packet(&[T_PTR, T_SRV, T_TXT, T_AAAA], &[], 0));
        if let Some(p) = goodbye {
            let _ = self.sock.send_to(&p, self.dest).await;
        }
    }
}

impl Drop for Mdns {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

async fn receive(sock: Arc<UdpSocket>, dest: SocketAddr, state: Arc<Mutex<State>>) {
    let mut buf = vec![0u8; 9000];
    loop {
        let Ok((n, _from)) = sock.recv_from(&mut buf).await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Some(msg) = parse(&buf[..n]) else { continue };
        let reply = {
            let mut st = state.lock().expect("not poisoned");
            if msg.response {
                if let Some(b) = st.browse.as_mut() {
                    b.learn(&msg.records);
                }
                None
            } else {
                st.ours.as_ref().and_then(|o| answer(o, &msg.questions))
            }
        };
        if let Some(p) = reply {
            let _ = sock.send_to(&p, dest).await;
        }
    }
}

/// Our answer to a query, if it asks about us.
fn answer(o: &Ours, questions: &[(String, u16)]) -> Option<Vec<u8>> {
    let mut answers: Vec<u16> = vec![];
    let mut add = |t: u16| {
        if !answers.contains(&t) {
            answers.push(t);
        }
    };
    let (instance, host) = (o.instance.to_ascii_lowercase(), o.host.to_ascii_lowercase());
    for (name, t) in questions {
        let any = *t == T_ANY;
        if name == SERVICE && (any || *t == T_PTR) {
            add(T_PTR);
        }
        if *name == instance {
            if any || *t == T_SRV {
                add(T_SRV);
            }
            if any || *t == T_TXT {
                add(T_TXT);
            }
        }
        if *name == host && (any || *t == T_AAAA || *t == T_A) {
            add(T_AAAA);
        }
    }
    if answers.is_empty() {
        return None;
    }
    let additional: Vec<u16> = [T_SRV, T_TXT, T_AAAA].into_iter().filter(|t| !answers.contains(t)).collect();
    Some(o.packet(&answers, &additional, 1))
}
