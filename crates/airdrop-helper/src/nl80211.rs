//! nl80211 over generic netlink: interfaces, access point, regulatory flags and
//! scan abort. Replaces every `iw` call of the Python helper.

use crate::genl::{find, Attrs, Msg, Socket};
use std::io;

// commands (linux/nl80211.h)
const CMD_GET_WIPHY: u8 = 1;
const CMD_GET_INTERFACE: u8 = 5;
const CMD_SET_INTERFACE: u8 = 6;
const CMD_NEW_INTERFACE: u8 = 7;
const CMD_DEL_INTERFACE: u8 = 8;
const CMD_GET_STATION: u8 = 17;
const CMD_START_AP: u8 = 15;
const CMD_STOP_AP: u8 = 16;
const CMD_TRIGGER_SCAN: u8 = 33;
const CMD_START_SCHED_SCAN: u8 = 75;
const CMD_STOP_SCHED_SCAN: u8 = 76;
const CMD_ABORT_SCAN: u8 = 114;
// attributes
const ATTR_WIPHY: u16 = 1;
const ATTR_IFINDEX: u16 = 3;
const ATTR_IFNAME: u16 = 4;
const ATTR_IFTYPE: u16 = 5;
const ATTR_WIPHY_BANDS: u16 = 22;
const ATTR_MNTR_FLAGS: u16 = 23;
const ATTR_BEACON_INTERVAL: u16 = 12;
const ATTR_DTIM_PERIOD: u16 = 13;
const ATTR_BEACON_HEAD: u16 = 14;
const ATTR_BEACON_TAIL: u16 = 15;
const ATTR_SSID: u16 = 52;
const ATTR_WIPHY_FREQ: u16 = 38;
const ATTR_WDEV: u16 = 153;
const ATTR_CHANNEL_WIDTH: u16 = 159;
const ATTR_CENTER_FREQ1: u16 = 160;
const ATTR_SPLIT_WIPHY_DUMP: u16 = 174;
const ATTR_SUPPORTED_IFTYPES: u16 = 32;
// nested
const BAND_ATTR_FREQS: u16 = 1;
const FREQ_ATTR_FREQ: u16 = 1;
const FREQ_ATTR_NO_IR: u16 = 3;
const MNTR_FLAG_ACTIVE: u16 = 6;

pub const IFTYPE_STATION: u32 = 2;
pub const IFTYPE_MONITOR: u32 = 6;
pub const IFTYPE_P2P_GO: u32 = 9;
// enum nl80211_chan_width
const WIDTH_20_NOHT: u32 = 0;
const WIDTH_40: u32 = 2;
pub const WIDTH_80: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// Legacy 20 MHz, no HT.
    NoHt,
    /// HT40 with the secondary channel above the primary.
    Ht40Plus,
    /// VHT 80 MHz around `center` (MHz), e.g. 5775 for channels 149-161.
    Vht80 { center: u32 },
}

#[derive(Debug, Clone)]
pub struct IfaceInfo {
    pub iftype: u32,
    /// Channel frequency in MHz while the interface holds a channel (associated, or an AP).
    pub freq: Option<u32>,
    /// `enum nl80211_chan_width` of that channel (3 = 80 MHz).
    pub width: Option<u32>,
}

/// What a scan event asks us to stop.
#[derive(Debug, Clone, Copy)]
pub enum ScanTarget {
    Ifindex(u32),
    Wdev(u64),
}

#[derive(Debug, Clone, Copy)]
pub struct ScanEvent {
    pub wiphy: u32,
    pub target: ScanTarget,
    pub scheduled: bool,
}

pub struct Nl80211 {
    sock: Socket,
}

fn ifindex_of(name: &str) -> io::Result<u32> {
    let c = std::ffi::CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: valid NUL-terminated string.
    match unsafe { libc::if_nametoindex(c.as_ptr()) } {
        0 => Err(io::Error::from(io::ErrorKind::NotFound)),
        n => Ok(n),
    }
}

impl Nl80211 {
    pub fn open() -> io::Result<Self> {
        Ok(Self { sock: Socket::open("nl80211")? })
    }

    pub fn interface(&mut self, name: &str) -> io::Result<IfaceInfo> {
        let ifindex = ifindex_of(name)?;
        let reply = self.sock.request(CMD_GET_INTERFACE, &Attrs::new().u32(ATTR_IFINDEX, ifindex))?;
        let msg = reply.first().ok_or_else(|| io::Error::other("no interface reply"))?;
        let attrs = msg.attrs();
        Ok(IfaceInfo {
            iftype: find(&attrs, ATTR_IFTYPE).and_then(|a| a.u32()).unwrap_or(0),
            freq: find(&attrs, ATTR_WIPHY_FREQ).and_then(|a| a.u32()),
            width: find(&attrs, ATTR_CHANNEL_WIDTH).and_then(|a| a.u32()),
        })
    }

    pub fn new_interface(&mut self, wiphy: u32, name: &str, iftype: u32, active: bool) -> io::Result<()> {
        let mut attrs = Attrs::new().u32(ATTR_WIPHY, wiphy).string(ATTR_IFNAME, name).u32(ATTR_IFTYPE, iftype);
        if active {
            attrs = attrs.nested(ATTR_MNTR_FLAGS, Attrs::new().flag(MNTR_FLAG_ACTIVE));
        }
        self.sock.request(CMD_NEW_INTERFACE, &attrs).map(|_| ())
    }

    pub fn del_interface(&mut self, name: &str) -> io::Result<()> {
        let ifindex = ifindex_of(name)?;
        self.sock.request(CMD_DEL_INTERFACE, &Attrs::new().u32(ATTR_IFINDEX, ifindex)).map(|_| ())
    }

    pub fn set_station_type(&mut self, name: &str) -> io::Result<()> {
        let ifindex = ifindex_of(name)?;
        self.sock
            .request(CMD_SET_INTERFACE, &Attrs::new().u32(ATTR_IFINDEX, ifindex).u32(ATTR_IFTYPE, IFTYPE_STATION))
            .map(|_| ())
    }

    /// Is the station associated? (One station entry on its interface.)
    pub fn associated(&mut self, name: &str) -> io::Result<bool> {
        let ifindex = ifindex_of(name)?;
        Ok(!self.sock.dump(CMD_GET_STATION, &Attrs::new().u32(ATTR_IFINDEX, ifindex))?.is_empty())
    }

    /// Start a beaconing access point (the hidden GO) on `freq`.
    pub fn start_ap(&mut self, name: &str, ssid: &str, freq: u32, width: Width, beacon_head: &[u8], beacon_tail: &[u8]) -> io::Result<()> {
        let ifindex = ifindex_of(name)?;
        let mut attrs = Attrs::new()
            .u32(ATTR_IFINDEX, ifindex)
            .u32(ATTR_BEACON_INTERVAL, 100)
            .u32(ATTR_DTIM_PERIOD, 1)
            .bytes(ATTR_BEACON_HEAD, beacon_head)
            .bytes(ATTR_SSID, ssid.as_bytes())
            .u32(ATTR_WIPHY_FREQ, freq);
        if !beacon_tail.is_empty() {
            attrs = attrs.bytes(ATTR_BEACON_TAIL, beacon_tail);
        }
        attrs = match width {
            Width::NoHt => attrs.u32(ATTR_CHANNEL_WIDTH, WIDTH_20_NOHT).u32(ATTR_CENTER_FREQ1, freq),
            Width::Ht40Plus => attrs.u32(ATTR_CHANNEL_WIDTH, WIDTH_40).u32(ATTR_CENTER_FREQ1, freq + 10),
            Width::Vht80 { center } => attrs.u32(ATTR_CHANNEL_WIDTH, WIDTH_80).u32(ATTR_CENTER_FREQ1, center),
        };
        self.sock.request(CMD_START_AP, &attrs).map(|_| ())
    }

    pub fn stop_ap(&mut self, name: &str) -> io::Result<()> {
        let ifindex = ifindex_of(name)?;
        self.sock.request(CMD_STOP_AP, &Attrs::new().u32(ATTR_IFINDEX, ifindex)).map(|_| ())
    }

    /// Does the phy mark `freq` No-IR (no initiating radiation)? Under regdom 00 all
    /// of 5 GHz is, and injected frames are then dropped silently.
    pub fn freq_is_no_ir(&mut self, wiphy: u32, freq: u32) -> io::Result<bool> {
        let reply = self.sock.dump(CMD_GET_WIPHY, &Attrs::new().u32(ATTR_WIPHY, wiphy).flag(ATTR_SPLIT_WIPHY_DUMP))?;
        for msg in &reply {
            let attrs = msg.attrs();
            let Some(bands) = find(&attrs, ATTR_WIPHY_BANDS) else { continue };
            for band in bands.children() {
                let battrs = band.children();
                let Some(freqs) = find(&battrs, BAND_ATTR_FREQS) else { continue };
                for f in freqs.children() {
                    let fattrs = f.children();
                    if find(&fattrs, FREQ_ATTR_FREQ).and_then(|a| a.u32()) == Some(freq) {
                        return Ok(find(&fattrs, FREQ_ATTR_NO_IR).is_some());
                    }
                }
            }
        }
        Ok(false)
    }

    pub fn supports_monitor(&mut self, wiphy: u32) -> io::Result<bool> {
        let reply = self.sock.dump(CMD_GET_WIPHY, &Attrs::new().u32(ATTR_WIPHY, wiphy).flag(ATTR_SPLIT_WIPHY_DUMP))?;
        Ok(reply.iter().any(|m| {
            find(&m.attrs(), ATTR_SUPPORTED_IFTYPES)
                .is_some_and(|t| t.children().iter().any(|c| u32::from(c.ty) == IFTYPE_MONITOR))
        }))
    }

    /// Stop the scan (or scheduled scan) a `ScanEvent` announced.
    pub fn stop_scan(&mut self, ev: &ScanEvent) -> io::Result<()> {
        let cmd = if ev.scheduled { CMD_STOP_SCHED_SCAN } else { CMD_ABORT_SCAN };
        let attrs = match ev.target {
            ScanTarget::Ifindex(i) => Attrs::new().u32(ATTR_IFINDEX, i),
            ScanTarget::Wdev(w) => Attrs::new().u64(ATTR_WDEV, w),
        };
        self.sock.request(cmd, &attrs).map(|_| ())
    }

    /// Abort whatever scan `name` is running; Ok(false) when there was none.
    pub fn abort_scan(&mut self, name: &str) -> io::Result<bool> {
        let ifindex = ifindex_of(name)?;
        match self.sock.request(CMD_ABORT_SCAN, &Attrs::new().u32(ATTR_IFINDEX, ifindex)) {
            Ok(_) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// A listener for scan-start notifications (the "scan" multicast group).
pub struct ScanEvents {
    sock: Socket,
}

impl ScanEvents {
    pub fn open() -> io::Result<Self> {
        let mut sock = Socket::open("nl80211")?;
        let group = sock.group_id("nl80211", "scan")?;
        sock.join(group)?;
        sock.set_timeout(std::time::Duration::from_millis(500))?;
        Ok(Self { sock })
    }

    /// Scan starts seen since the last call (empty on timeout).
    pub fn poll(&mut self) -> io::Result<Vec<ScanEvent>> {
        let Some(msgs) = self.sock.next_event()? else { return Ok(vec![]) };
        Ok(msgs.iter().filter_map(scan_event).collect())
    }
}

fn scan_event(msg: &Msg) -> Option<ScanEvent> {
    let scheduled = match msg.cmd {
        CMD_TRIGGER_SCAN => false,
        CMD_START_SCHED_SCAN => true,
        _ => return None,
    };
    let attrs = msg.attrs();
    let wiphy = find(&attrs, ATTR_WIPHY)?.u32()?;
    let target = match (find(&attrs, ATTR_IFINDEX).and_then(|a| a.u32()), find(&attrs, ATTR_WDEV).and_then(|a| a.u64())) {
        (Some(i), _) => ScanTarget::Ifindex(i),
        (None, Some(w)) => ScanTarget::Wdev(w),
        _ => return None,
    };
    Some(ScanEvent { wiphy, target, scheduled })
}
