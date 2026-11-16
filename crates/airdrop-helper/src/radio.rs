//! The radio side: monitor vifs, the hidden P2P-GO that holds the channel,
//! mt76 power-saving knobs, and the checks that the radio really works.

use crate::nl80211::{Nl80211, Width};
use crate::out::log;
use crate::sys::{self, DEBUGFS};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

pub const AWDL_IFACE: &str = "awdl0";
/// Plain monitor vif: channel + RX.
pub const MON_IFACE: &str = "awdlmon";
/// Active monitor vif: the chip ACKs unicast to its MAC, OWL injects through it.
pub const ACT_IFACE: &str = "awdlact";
/// Hidden P2P-GO that holds the radio's only active channel context in borrow mode.
pub const GO_IFACE: &str = "awdlgo";
pub const GO_SSID: &str = "DIRECT-ad";
pub const AWDL_CHANNELS: [u32; 4] = [6, 36, 44, 149];
/// Channels the borrow GO may sit on: the social channels, and 36.
/// 36 is not a social channel: it is for an iPhone joined to a network on 36, which gives the
/// social channels 2 of 16 slots and its own channel 7 (EXPERIMENTS C-2, E2.11).
pub const GO_CHANNELS: [u32; 4] = [6, 36, 44, 149];

/// Random locally administered unicast MAC.
pub fn random_go_mac() -> std::io::Result<[u8; 6]> {
    let mut b = [0u8; 6];
    sys::random_bytes(&mut b)?;
    b[0] = (b[0] & 0xFC) | 0x02;
    Ok(b)
}

pub fn mac_string(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Minimal beacon head: header, fixed fields, hidden (empty) SSID, supported rates
/// (+ DS parameter set on 2.4 GHz). mac80211 adds the TIM.
pub fn beacon_head(mac: &[u8; 6], channel: u32) -> Vec<u8> {
    let mut b = vec![0x80, 0x00, 0x00, 0x00];
    b.extend([0xff; 6]);
    b.extend(mac);
    b.extend(mac);
    b.extend([0x00, 0x00]);
    b.extend([0u8; 8]);
    b.extend(100u16.to_le_bytes());
    b.extend(0x0001u16.to_le_bytes()); // ESS
    b.extend([0x00, 0x00]); // empty SSID
    if channel <= 14 {
        b.extend([0x01, 0x08, 0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]);
        b.extend([0x03, 0x01, channel as u8]);
    } else {
        b.extend([0x01, 0x08, 0x8C, 0x12, 0x98, 0x24, 0xB0, 0x48, 0x60, 0x6C]);
    }
    b
}

/// The part of the beacon after the TIM that says how wide the network runs: HT Capabilities
/// and HT Operation, and at 80 MHz VHT Capabilities and VHT Operation, with what OWL
/// advertises in AWDL (2 streams, short GI, no beamforming). Without them the MT7921 firmware
/// sends to a station of this GO at 20 MHz whatever the station can do (E-TXAGG, 2026-11-16).
pub fn beacon_tail(channel: u32, width: Width) -> Vec<u8> {
    let mut b = Vec::new();
    if matches!(width, Width::NoHt) {
        return b;
    }
    // HT Capabilities: LDPC, 20/40 MHz, short GI 20 and 40, max A-MSDU 7935; A-MPDU 64 KiB,
    // density 6; MCS 0-15.
    b.extend([45, 26]);
    b.extend(0x09efu16.to_le_bytes());
    b.push(0x1b);
    b.extend([0xff, 0xff]);
    b.extend([0u8; 14 + 2 + 4 + 1]);
    // HT Operation: the primary channel, where the secondary 20 MHz lies, any width allowed.
    let above = matches!(channel, 36 | 44 | 149 | 157);
    b.extend([61, 22, channel as u8, if above { 0x05 } else { 0x07 }]);
    b.extend([0u8; 20]);
    if let Width::Vht80 { center } = width {
        // VHT Capabilities as in OWL's TLV 0x11: RX LDPC, short GI 80, A-MPDU exponent 3;
        // MCS 0-9 on 2 streams both ways.
        b.extend([191, 12, 0x30, 0x00, 0x80, 0x01, 0xfa, 0xff, 0x00, 0x00, 0xfa, 0xff, 0x00, 0x00]);
        // VHT Operation: 80 MHz around the centre channel, basic set MCS 0-7 on one stream.
        let centre = sys::channel_of_freq(center).unwrap_or(0) as u8;
        b.extend([192, 5, 1, centre, 0, 0xfc, 0xff]);
    }
    b
}

/// Create a monitor vif (replacing a leftover of the same name).
pub fn add_monitor(nl: &mut Nl80211, wiphy: u32, name: &str, active: bool) -> io::Result<()> {
    if sys::iface_exists(name) {
        let _ = nl.del_interface(name);
    }
    nl.new_interface(wiphy, name, crate::nl80211::IFTYPE_MONITOR, active)
}

/// Give a new vif its MAC and bring it up. The MAC is not optional: mac80211 sends an
/// injected frame through the interface whose address equals the frame's transmitter,
/// so a vif without the GO's MAC transmits almost nothing (seen 2026-11-12).
pub fn bring_up(name: &str, mac: &[u8; 6]) -> io::Result<()> {
    sys::set_mac(name, mac)?;
    sys::set_link_up(name)
}

/// Create the hidden GO and start it beaconing on `channel`; returns its MAC.
/// `fast` retries the start while a scan still holds the radio instead of waiting first.
/// Centre of the 80 MHz block a 5 GHz channel belongs to (only the blocks AWDL uses).
fn vht80_center(channel: u32) -> Option<u32> {
    match channel {
        36 | 40 | 44 | 48 => Some(sys::freq_of_channel(42)),
        149 | 153 | 157 | 161 => Some(sys::freq_of_channel(155)),
        _ => None,
    }
}

/// Start the hidden GO that holds the radio on `channel`. Returns its MAC and whether it
/// runs at 80 MHz: only then may OWL advertise VHT (the iPhone would send 80 MHz frames
/// a 40 MHz radio cannot receive).
pub fn start_go(nl: &mut Nl80211, wiphy: u32, channel: u32, narrow: bool, vht80: bool, fast: bool, full_beacon: bool) -> Result<([u8; 6], bool), String> {
    let freq = sys::freq_of_channel(channel);
    // A new address per session, on purpose. One address per boot was tried on 2026-11-16: the
    // iPhone then stopped asking for a Block Ack session after a few sessions (none in a 3-minute
    // session, 3 MB/s instead of 21) and discovery was no better (EXPERIMENTS C-3).
    let mac = random_go_mac().map_err(|e| format!("no random MAC for the GO: {e}"))?;
    nl.new_interface(wiphy, GO_IFACE, crate::nl80211::IFTYPE_P2P_GO, false).map_err(|e| format!("add {GO_IFACE}: {e}"))?;
    sys::set_mac(GO_IFACE, &mac).map_err(|e| format!("set mac on {GO_IFACE}: {e}"))?;
    sys::set_link_up(GO_IFACE).map_err(|e| format!("{GO_IFACE} up: {e}"))?;
    // HT40 by default on a 5 GHz borrow channel: hold a 40 MHz chanctx so we receive the
    // iPhone's HT40 frames (300 Mbit/s; E2.10). 2.4 GHz stays 20 MHz; --narrow forces it.
    // VHT80 (E-VHT80, 2026-11-14): the 80 MHz block, so the iPhone's VHT80 frames (MCS 9,
    // 2 streams) are received: 14.8 -> 21.0 MB/s on 150.9 MB.
    let mut width = match (freq >= 5000 && !narrow, vht80.then(|| vht80_center(channel)).flatten()) {
        (true, Some(center)) => Width::Vht80 { center },
        (true, None) => Width::Ht40Plus,
        (false, _) => Width::NoHt,
    };
    let head = beacon_head(&mac, channel);
    let attempts = if fast { 6 } else { 1 };
    for attempt in 1..=attempts {
        let tail = if full_beacon { beacon_tail(channel, width) } else { vec![] };
        let mut result = nl.start_ap(GO_IFACE, GO_SSID, freq, width, &head, &tail);
        if let (Err(e), Width::Vht80 { .. }) = (&result, width) {
            // Refused at 80 MHz (regulatory rules, driver): 40 MHz still works. Tried at once,
            // so a busy radio (which refuses both) is told apart from a refused width.
            let refused = e.to_string();
            let tail = if full_beacon { beacon_tail(channel, Width::Ht40Plus) } else { vec![] };
            result = nl.start_ap(GO_IFACE, GO_SSID, freq, Width::Ht40Plus, &head, &tail);
            if result.is_ok() {
                log(&format!("borrow: 80 MHz refused on channel {channel} ({refused}): 40 MHz"));
                width = Width::Ht40Plus;
            }
        }
        match result {
            Ok(()) => break,
            Err(e) if attempt == attempts => return Err(format!("start access point on {GO_IFACE}: {e}")),
            Err(e) => {
                log(&format!("ap start attempt {attempt}: {e}")); // busy while a scan still runs
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    let info = loop {
        let info = nl.interface(GO_IFACE).ok();
        if let Some(i) = info.filter(|i| i.freq == Some(freq)) {
            break i;
        }
        if Instant::now() >= deadline {
            return Err(format!("the GO did not take channel {channel}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // VHT only on the kernel's word that the channel is 80 MHz wide.
    let wide80 = matches!(width, Width::Vht80 { .. }) && info.width == Some(crate::nl80211::WIDTH_80);
    if matches!(width, Width::Vht80 { .. }) && !wide80 {
        log(&format!("borrow: the GO is not 80 MHz wide (width {:?}): no VHT", info.width));
    }
    log(&format!("borrow: GO {GO_IFACE} {} up on {freq} MHz{}", mac_string(&mac), if wide80 { ", 80 MHz" } else { "" }));
    Ok((mac, wide80))
}

// ---- mt76 power saving ----

pub fn mt76_dir(phy: &str) -> Option<String> {
    let d = format!("{DEBUGFS}/ieee80211/{phy}/mt76");
    std::path::Path::new(&d).is_dir().then_some(d)
}

pub const PM_KNOBS: [&str; 2] = ["runtime-pm", "deep-sleep"];

// ---- RX checks ----

pub fn receives(iface: &str, timeout: Duration) -> bool {
    let path = format!("/sys/class/net/{iface}/statistics/rx_packets");
    let Some(start) = sys::read_int(&path) else { return true }; // cannot tell: do not block
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        match sys::read_int(&path) {
            Some(n) if n > start => return true,
            Some(_) => {}
            None => return true,
        }
    }
    false
}

fn packet_socket(iface: &str, timeout: Duration) -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2); the fd is owned immediately.
    let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW | libc::SOCK_CLOEXEC, i32::from((libc::ETH_P_ALL as u16).to_be())) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is valid and exclusively ours.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let name = std::ffi::CString::new(iface).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: valid NUL-terminated string.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: zeroed sockaddr_ll is valid; we set the fields bind needs.
    let mut sa: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    sa.sll_family = libc::AF_PACKET as u16;
    sa.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
    sa.sll_ifindex = index as i32;
    // SAFETY: sa is a valid sockaddr_ll of the stated size.
    if unsafe { libc::bind(fd.as_raw_fd(), std::ptr::from_ref(&sa).cast(), size_of::<libc::sockaddr_ll>() as u32) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let tv = libc::timeval { tv_sec: timeout.as_secs() as _, tv_usec: timeout.subsec_micros() as _ };
    // SAFETY: tv outlives the call and the length matches.
    unsafe {
        libc::setsockopt(fd.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVTIMEO, std::ptr::from_ref(&tv).cast(), size_of::<libc::timeval>() as u32);
    }
    Ok(fd)
}

fn recv_frame(fd: &OwnedFd, buf: &mut [u8]) -> Option<(usize, u8)> {
    // SAFETY: zeroed sockaddr_ll is a valid out-parameter; buf is writable for its length.
    let mut sa: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_ll>() as u32;
    let n = unsafe { libc::recvfrom(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0, std::ptr::from_mut(&mut sa).cast(), &mut len) };
    (n >= 0).then_some((n as usize, sa.sll_pkttype))
}

/// Channel frequency (MHz) in a radiotap header, if it carries one.
fn radiotap_freq(frame: &[u8]) -> Option<u32> {
    let word = |off: usize| Some(u32::from_le_bytes(frame.get(off..off + 4)?.try_into().ok()?));
    let present = word(4)?;
    if present & (1 << 3) == 0 {
        return None;
    }
    // Skip the chained "present" words; fields start after the last one.
    let mut off = 8;
    let mut w = present;
    while w & (1 << 31) != 0 {
        w = word(off)?;
        off += 4;
    }
    if present & 1 != 0 {
        off = off.next_multiple_of(8) + 8; // TSFT
    }
    off += (present >> 1 & 1) as usize + (present >> 2 & 1) as usize; // flags, rate
    off = off.next_multiple_of(2);
    Some(u32::from(u16::from_le_bytes(frame.get(off..off + 2)?.try_into().ok()?)))
}

/// Most common radiotap frequency heard on a monitor iface (up to 20 frames or
/// 4 s); with `want`, that frequency as soon as any frame was heard on it.
pub fn heard_frequency(iface: &str, want: Option<u32>) -> Option<u32> {
    let fd = packet_socket(iface, Duration::from_millis(250)).ok()?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut counts = std::collections::BTreeMap::<u32, u32>::new();
    let mut buf = vec![0u8; 4096];
    let mut frames = 0;
    while Instant::now() < deadline && frames < 20 {
        let Some((n, _)) = recv_frame(&fd, &mut buf) else { continue };
        if let Some(f) = radiotap_freq(&buf[..n]) {
            *counts.entry(f).or_default() += 1;
            frames += 1;
            if want == Some(f) {
                return want;
            }
        }
    }
    if counts.is_empty() {
        log(&format!("heard on {iface}: nothing"));
        return None;
    }
    log(&format!("heard on {iface}: {}", counts.iter().map(|(f, c)| format!("{f} MHz x{c}")).collect::<Vec<_>>().join(", ")));
    counts.into_iter().max_by_key(|&(_, c)| c).map(|(f, _)| f)
}

/// Do frames injected on `tx` really leave? mac80211 reflects a frame that reached
/// the driver back to monitor interfaces; one dropped earlier (No-IR channel, a kernel
/// that changed the injection path) is never reflected. `None` = could not test.
pub fn injection_works(tx: &str, rx: &str, mac: &[u8; 6]) -> Option<bool> {
    let mut marker = [0u8; 6];
    sys::random_bytes(&mut marker).ok()?;
    // Vendor-specific action frame with a private OUI (not Apple's), to a made-up unicast
    // address. Unicast on purpose: the monitor only gets back frames the chip reports a TX
    // status for, and that is frames that ask for an ACK (acked or not). A broadcast no-ack
    // frame never came back with the HT driver build, so the check warned on every working
    // session (2026-11-12).
    let mut frame = vec![0, 0, 8, 0, 0, 0, 0, 0, 0xd0, 0, 0, 0];
    frame.extend([0x02, 0x41, 0x44, 0x43, 0x48, 0x4b]);
    frame.extend(mac);
    frame.extend(mac);
    frame.extend([0, 0, 0x7f, 0x02, 0x41, 0x44]);
    frame.extend(b"AIRDROP-INJECT-CHECK");
    frame.extend(marker);
    let (Ok(rx_fd), Ok(tx_fd)) = (packet_socket(rx, Duration::from_millis(100)), packet_socket(tx, Duration::from_millis(100))) else {
        log("injection check: cannot open sockets");
        return None;
    };
    let mut buf = vec![0u8; 4096];
    for _ in 0..4 {
        // SAFETY: frame is a valid readable buffer.
        if unsafe { libc::send(tx_fd.as_raw_fd(), frame.as_ptr().cast(), frame.len(), 0) } < 0 {
            log(&format!("injection check: send failed ({})", io::Error::last_os_error()));
            return Some(false);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Some((n, pkttype)) = recv_frame(&rx_fd, &mut buf) {
                // PACKET_OUTGOING (4) is our own send, tapped on the same device
                if pkttype != 4 && buf[..n].windows(marker.len()).any(|w| w == marker) {
                    log(&format!("injection check: frame on {tx} came back on {rx}"));
                    return Some(true);
                }
            }
        }
    }
    Some(false)
}
