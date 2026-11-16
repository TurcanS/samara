#!/usr/bin/env python3
"""Measure one or more airdrop-trace.sh sessions: outcome, speed, and what the radio did.

usage: python3 -I scripts/analyze-trace.py SESSION_DIR... [--csv FILE] [-v]

One block per session (or one CSV row with --csv, appended, header written once):

  outcome     the client's own verdict, from send.log / receive.log
  air time    seconds in which data frames flowed back to back (gaps < 5 ms),
              and that as a share of the transfer: the duty cycle the iPhone gave us
  frame gap   median spacing of full-size data frames inside a burst. It is the
              cost of one frame on air: ~2.3 ms = 6 Mbit/s, ~1.1 ms = 12 Mbit/s,
              ~0.65 ms = 24 Mbit/s, ~0.4 ms = 54 Mbit/s, ~0.3 ms = iPhone HT40 without aggregation
  MAC loss    our unicast frames the peer did not acknowledge (radiotap TX status)
  block ack   ADDBA requests from the peer, our answers, Block Acks we sent
  off channel frames the monitor heard on other channels during the transfer, and Wi-Fi scans
              in iw-event.log: a scan takes the radio off the AWDL channel (~130 ms per channel)
  reachable   connection attempts the peer's radio acknowledged vs TCP replies from it.
              "acked, no reply" = the iPhone is in range but AirDrop is not receiving
              (set "Everyone for 10 Minutes" again)

Our AWDL MAC is read from the session's iface.txt (in borrow mode it is the GO's
random MAC, not the station's). No dependencies beyond the standard library.
"""
import argparse
import collections
import csv
import re
import statistics
import struct
import subprocess
import sys
from pathlib import Path

AWDL_BSSID = bytes.fromhex("002500ff9473")
BURST_GAP = 0.005   # frames closer than this belong to one burst
BIG = 1000          # bytes: a full-size data frame


def radiotap(rt):
    """The few radiotap fields we need: tx flags, MCS (HT or VHT), rate."""
    out = {}
    if len(rt) < 8:
        return out
    off, pres = 4, []
    while off + 4 <= len(rt):
        p = struct.unpack_from("<I", rt, off)[0]
        pres.append(p)
        off += 4
        if not p & (1 << 31):
            break
    sizes = {0: (8, 8), 1: (1, 1), 2: (1, 1), 3: (4, 2), 4: (2, 2), 5: (1, 1), 6: (1, 1), 7: (2, 2), 8: (2, 2),
             9: (2, 2), 10: (1, 1), 11: (1, 1), 12: (1, 1), 13: (1, 1), 14: (2, 2), 15: (2, 2), 16: (1, 1),
             17: (1, 1), 18: (8, 4), 19: (3, 1), 20: (8, 4), 21: (12, 2)}
    for bit in range(22):
        if not pres[0] & (1 << bit):
            continue
        size, align = sizes[bit]
        off = (off + align - 1) // align * align
        if off + size > len(rt):
            break
        if bit == 2:
            out["rate"] = rt[off] / 2
        elif bit == 3:
            out["freq"] = struct.unpack_from("<H", rt, off)[0]
        elif bit == 15:
            out["txflags"] = struct.unpack_from("<H", rt, off)[0]
        elif bit == 19:
            out["mcs"] = rt[off + 2]
            out["bw40"] = bool(rt[off + 1] & 0x01)
        elif bit == 20:
            out["ampdu"] = True
        elif bit == 21:  # VHT: known, flags, bandwidth, mcs_nss[4], ...
            bw = {0: 20, 1: 40, 4: 80, 11: 160}.get(rt[off + 3], rt[off + 3])
            user = rt[off + 4]
            out["vht"] = (user >> 4, user & 0x0F, bw)
        off += size
    return out


def frames(path):
    data = path.read_bytes()
    off = 24
    while off + 16 <= len(data):
        ts, tu, incl, orig = struct.unpack_from("<IIII", data, off)
        pkt = data[off + 16:off + 16 + incl]
        off += 16 + incl
        if len(pkt) < 8:
            continue
        rl = struct.unpack_from("<H", pkt, 2)[0]
        if len(pkt) - rl >= 10:
            yield ts + tu / 1e6, pkt[:rl], pkt[rl:], orig - rl


def our_mac(session):
    """awdl0's address: the first link/ether in iface.txt."""
    try:
        m = re.search(r"link/ether ([0-9a-f:]{17})", (session / "iface.txt").read_text(errors="replace"))
    except OSError:
        m = None
    return bytes.fromhex(m.group(1).replace(":", "")) if m else None


def ip6(frame):
    """(next header, payload) of the IPv6 packet in an AWDL data frame, or None."""
    i = frame.find(b"\x86\xdd", 24, 56)
    if i < 0 or len(frame) < i + 42:
        return None
    return frame[i + 8], frame[i + 42:]


def bursts(times):
    """(seconds spent in back-to-back bursts, median in-burst gap) for sorted timestamps."""
    gaps = [b - a for a, b in zip(times, times[1:])]
    inside = [g for g in gaps if g < BURST_GAP]
    return sum(inside), (statistics.median(inside) if inside else None)


def client_verdict(session):
    """(direction, outcome, MB, seconds) from the client's log."""
    for name, direction in (("send.log", "send"), ("receive.log", "receive"), ("find.log", "find")):
        log = session / name
        if not log.exists():
            continue
        text = log.read_text(errors="replace")
        pattern = (r"Sent ([\d.]+) (KB|MB|GB) to .* in ([\d.]+)s" if direction == "send"
                   else r"Received ([\d.]+) (KB|MB|GB) from .* in ([\d.]+)s")
        done = re.findall(pattern, text)
        if done:  # several files in one session: totals
            mb = sum(float(n) * {"KB": 1e-3, "MB": 1, "GB": 1e3}[unit] for n, unit, _ in done)
            return direction, "ok" if len(done) == 1 else f"ok x{len(done)}", mb, sum(float(t) for _, _, t in done)
        for needle, verdict in (("stalled", "stalled"), ("declined", "declined"), ("Send failed", "failed"),
                                ("No matching device", "not found"), ("No devices found", "not found"),
                                ("Traceback", "crashed")):
            if needle in text:
                return direction, verdict, None, None
        return direction, "no transfer", None, None
    return "?", "no client log", None, None


def analyse(session):
    row = collections.OrderedDict(session=session.name)
    direction, outcome, mb, secs = client_verdict(session)
    row.update(direction=direction, outcome=outcome, mb=mb, seconds=secs,
               kb_per_s=round(mb * 1000 / secs, 1) if mb and secs else None)
    me = our_mac(session)
    air = session / "air.pcap"
    if me is None or not air.exists():
        row["note"] = "no air.pcap or iface.txt"
        return row
    fr = list(frames(air))
    if not fr:
        row["note"] = "empty air.pcap"
        return row

    # The peer: whoever exchanges the most unicast with us; else the loudest AWDL node.
    talk = collections.Counter()
    heard = collections.Counter()
    for _, _, w, _ in fr:
        if len(w) < 24:
            continue
        ra, ta = w[4:10], w[10:16]
        if w[0] == 0xD0 and w[16:22] == AWDL_BSSID and ta != me and w[24:28] == b"\x7f\x00\x17\xf2":
            heard[ta] += 1
        if (w[0] & 0x0C) == 0x08 and not ra[0] & 1:
            if ta == me:
                talk[ra] += 1
            elif ra == me:
                talk[ta] += 1
    row["awdl_peers"] = len(heard)
    peer = (talk or heard).most_common(1)
    if not peer:
        row["note"] = "no AWDL peer heard"
        return row
    peer = peer[0][0]
    row["peer"] = peer.hex(":")

    tx_big, rx_big = [], []
    tx_total = tx_lost = 0
    syn_acked = syn_lost = tcp_from_peer = pong = 0
    addba_req = addba_resp = ba_sent = ampdu = 0
    rx_mcs = collections.Counter()
    for t, rt, w, size in fr:
        if len(w) < 24:
            continue
        ra, ta = w[4:10], w[10:16]
        kind = w[0]
        if kind == 0x94 and ta == me:          # Block Ack we (the chip) sent
            ba_sent += 1
        if kind == 0xD0 and len(w) > 26 and w[24] == 3:
            if ta == peer and ra == me and w[25] == 0:
                addba_req += 1
            elif ta == me and ra == peer and w[25] == 1:
                addba_resp += 1
        if (kind & 0x0C) != 0x08:
            continue
        tap = radiotap(rt)
        if ta == me and ra == peer:
            lost = bool(tap.get("txflags", 0) & 1)
            tx_total += 1
            tx_lost += lost
            if size >= BIG:
                tx_big.append(t)
            pkt = ip6(w)
            if pkt and pkt[0] == 6 and len(pkt[1]) > 13 and pkt[1][13] & 0x3F == 0x02:  # SYN
                syn_lost += lost
                syn_acked += not lost
        elif ta == peer and ra == me:
            if size >= BIG:
                rx_big.append(t)
                if "vht" in tap:
                    mcs, nss, bw = tap["vht"]
                    rx_mcs[f"VHT MCS{mcs} x{nss}/{bw}"] += 1
                elif "mcs" in tap:
                    rx_mcs[f"MCS{tap['mcs']}/{40 if tap.get('bw40') else 20}"] += 1
            if tap.get("ampdu"):
                ampdu += 1
            pkt = ip6(w)
            if pkt and pkt[0] == 6:
                tcp_from_peer += 1
            elif pkt and pkt[0] == 58 and pkt[1][:1] == b"\x81":
                pong += 1

    data = tx_big if len(tx_big) >= len(rx_big) else rx_big
    if len(data) > 10:
        busy, gap = bursts(data)
        span = data[-1] - data[0]
        row.update(data_frames=len(data), air_seconds=round(busy, 2),
                   duty_pct=round(100 * busy / span, 1) if span else None,
                   frame_gap_ms=round(gap * 1000, 2) if gap else None)
    if len(data) > 10:
        # Frames of other stations inside the transfer, by channel: ours is the one data flows on.
        freqs = collections.Counter(radiotap(rt).get("freq") for t, rt, w, _ in fr
                                    if data[0] <= t <= data[-1] and len(w) >= 16 and w[10:16] != me)
        freqs.pop(None, None)
        if freqs:
            home = freqs.most_common(1)[0][0]
            row.update(off_channel_frames=sum(n for f, n in freqs.items() if f != home),
                       off_channels=len(freqs) - 1)
    try:
        events = (session / "iw-event.log").read_text(errors="replace")
        row.update(scans_started=len(re.findall(r"scan started", events)),
                   scans_aborted=len(re.findall(r"scan aborted", events)))
    except OSError:
        pass
    row.update(tx_frames=tx_total, mac_loss_pct=round(100 * tx_lost / tx_total, 1) if tx_total else None,
               addba_requests=addba_req, addba_answers=addba_resp, block_acks_sent=ba_sent,
               ampdu_frames=ampdu, syn_acked=syn_acked, syn_lost=syn_lost, tcp_from_peer=tcp_from_peer,
               ping_replies=pong)
    if rx_mcs:
        total = sum(rx_mcs.values())
        row["peer_rate"] = ", ".join(f"{rate} ({n * 100 // total} %)" for rate, n in rx_mcs.most_common(3))
    if syn_acked and not tcp_from_peer:
        row["note"] = "peer acknowledged our connection attempts at radio level but never replied: " \
                      "AirDrop on it is not receiving (set Everyone for 10 Minutes again)"
    elif not tx_total and direction != "receive":
        row["note"] = "nothing of ours reached the air towards the peer"
    try:
        owl = (session / "owl.log").read_text(errors="replace")
        slots = re.findall(r"(\d+)/16 slots on our channel", owl)
        if slots:
            row["slots_max"] = max(map(int, slots))
        row["no_overlap"] = owl.count("NO OVERLAP")
        m = re.search(r"owl -i \S+ (.*)", (session / f"{direction}.log").read_text(errors="replace"))
        if m:
            row["owl_flags"] = m.group(1).strip()
    except OSError:
        pass
    return row


def show(row, verbose, session):
    g = row.get
    print(f"== {row['session']}  [{g('direction')}]  {g('outcome')}"
          + (f": {g('mb'):.1f} MB in {g('seconds'):.1f} s = {g('kb_per_s')} KB/s" if g("kb_per_s") else ""))
    if g("peer"):
        print(f"   peer {g('peer')} ({g('awdl_peers')} AWDL node(s) heard)"
              + (f", sends to us at {g('peer_rate')}" if g("peer_rate") else "")
              + (f", up to {g('slots_max')}/16 slots on our channel" if g("slots_max") is not None else ""))
    if g("data_frames"):
        print(f"   air time   {g('air_seconds')} s in bursts = {g('duty_pct')} % duty; "
              f"frame gap {g('frame_gap_ms')} ms over {g('data_frames')} full frames")
    if g("tx_frames"):
        print(f"   MAC loss   {g('mac_loss_pct')} % of {g('tx_frames')} unicast frames we sent")
    if g("addba_requests") or g("addba_answers") or g("block_acks_sent") or g("ampdu_frames"):
        print(f"   block ack  {g('addba_requests')} requests from the peer, {g('addba_answers')} answered, "
              f"{g('block_acks_sent')} Block Acks sent by us, {g('ampdu_frames')} frames marked A-MPDU")
    if g("off_channel_frames") or g("scans_started"):
        print(f"   off channel {g('off_channel_frames') or 0} frames heard on {g('off_channels') or 0} other channels during the transfer"
              + (f"; {g('scans_started')} Wi-Fi scans started, {g('scans_aborted')} aborted"
                 if g("scans_started") is not None else ""))
    if g("syn_acked") or g("syn_lost"):
        print(f"   reachable  {g('syn_acked')} connection attempts acked by its radio ({g('syn_lost')} lost), "
              f"{g('tcp_from_peer')} TCP frames back, {g('ping_replies')} ping replies")
    if g("owl_flags"):
        print(f"   owl        {g('owl_flags')}")
    if g("note"):
        print(f"   NOTE       {g('note')}")
    if verbose and (session / "awdl0.pcap").exists():
        out = subprocess.run(["tcpdump", "-r", str(session / "awdl0.pcap"), "-nn"],
                             capture_output=True, text=True).stdout.splitlines()
        for line in [l for l in out if ".877" in l or "_airdrop" in l][:25]:
            print("     ", line[:170])


def main():
    ap = argparse.ArgumentParser(description="Measure airdrop-trace.sh sessions (see the header of this file).")
    ap.add_argument("sessions", nargs="+", type=Path)
    ap.add_argument("--csv", type=Path, help="append one row per session to this file")
    ap.add_argument("-v", "--verbose", action="store_true", help="also list the TCP/mDNS lines from awdl0.pcap")
    args = ap.parse_args()
    rows = []
    for session in args.sessions:
        if not session.is_dir():
            print(f"{session}: not a session directory", file=sys.stderr)
            continue
        row = analyse(session)
        rows.append(row)
        show(row, args.verbose, session)
    if args.csv and rows:
        fields = ["session", "direction", "outcome", "mb", "seconds", "kb_per_s", "duty_pct", "air_seconds",
                  "frame_gap_ms", "data_frames", "tx_frames", "mac_loss_pct", "peer_rate", "slots_max",
                  "addba_requests", "addba_answers", "block_acks_sent", "ampdu_frames", "syn_acked", "syn_lost",
                  "tcp_from_peer", "ping_replies", "awdl_peers", "no_overlap", "owl_flags", "peer", "note",
                  "off_channel_frames", "off_channels", "scans_started", "scans_aborted"]  # new columns go last
        new = not args.csv.exists()
        with args.csv.open("a", newline="") as fh:
            out = csv.DictWriter(fh, fieldnames=fields, extrasaction="ignore")
            if new:
                out.writeheader()
            out.writerows(rows)
        print(f"\n{len(rows)} row(s) appended to {args.csv}")


if __name__ == "__main__":
    main()
