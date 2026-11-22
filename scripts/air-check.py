#!/usr/bin/env python3
"""What holds a Wi-Fi card back during an AirDrop transfer, read from one trace.

    NAME=send DUR=60 scripts/airdrop-trace.sh send --mode borrow -r "device name" FILE
    python3 -I scripts/air-check.py probe/sessions/<time>-send

It reads the trace folder (air.pcap: the radio seen by our monitor interface; awdl0.pcap: the IP traffic;
owl.log; iface.txt or helper.log) and prints what the radio did, what TCP saw, and the likely cause when it is slow. The
patterns come from measured cases (docs/wiki/Faster-on-your-card.md):

  - our frames leave in short bursts with long silences while the peer is there: the driver holds frames for
    a peer it has no station entry for (Realtek rtw89 before the station path: 17 frames every ~140 ms);
  - the peer retries most of its frames to us: we do not acknowledge them in time;
  - our driver sends Block Ack requests in mac80211's format: the iPhone ignores them, the queue stops;
  - nothing acknowledged at all: our frames never reach the air (rate the driver drops, No-IR channel).

Standard library only. Our address and the peer's are read from iface.txt and owl.log; --me/--peer override.
"""
import argparse
import bisect
import collections
import re
import struct
import sys
from pathlib import Path

AWDL_BSSID = bytes.fromhex('002500ff9473')
TU = 1024e-6
SLOT = 16 * TU


def pcap_frames(path):
    """(time in s, linktype, bytes) for every record of a classic pcap file (as tcpdump -w writes)."""
    with open(path, 'rb') as f:
        head = f.read(24)
        if len(head) < 24:
            return
        magic = head[:4]
        if magic in (b'\xd4\xc3\xb2\xa1', b'\x4d\x3c\xb2\xa1'):
            end = '<'
        elif magic in (b'\xa1\xb2\xc3\xd4', b'\xa1\xb2\x3c\x4d'):
            end = '>'
        else:
            raise SystemExit(f'{path}: not a classic pcap file (pcapng is not read)')
        nano = magic in (b'\x4d\x3c\xb2\xa1', b'\xa1\xb2\x3c\x4d')
        linktype = struct.unpack(end + 'I', head[20:24])[0]
        while True:
            rec = f.read(16)
            if len(rec) < 16:
                return
            sec, frac, caplen, _ = struct.unpack(end + 'IIII', rec)
            data = f.read(caplen)
            yield sec + frac / (1e9 if nano else 1e6), linktype, data


def radiotap(data):
    """(TSF in s or None, offset of the 802.11 header)."""
    if len(data) < 8:
        return None, len(data)
    length = struct.unpack('<H', data[2:4])[0]
    present = struct.unpack('<I', data[4:8])[0]
    tsf = None
    if present & 1:
        off, word = 8, present
        while word & 0x80000000 and off + 4 <= length:
            word = struct.unpack('<I', data[off:off + 4])[0]
            off += 4
        off = (off + 7) & ~7
        if off + 8 <= length:
            tsf = struct.unpack('<Q', data[off:off + 8])[0] / 1e6
    return tsf, length


def mac(text):
    return bytes.fromhex(text.replace(':', '')) if text else None


def addresses(folder):
    """(our AWDL address, the peer's) from iface.txt and owl.log."""
    me = peer = None
    iface = folder / 'iface.txt'
    if iface.exists():
        text = iface.read_text(errors='replace')
        for name in ('awdlgo', 'awdlact', 'awdlmon'):
            m = re.search(rf'\d+: {name}:.*?link/ether ([0-9a-f:]{{17}})', text, re.S)
            if m:
                me = m[1]
                break
    helper = folder / 'helper.log'
    if not me and helper.exists():
        found = re.findall(r'GO awdlgo ([0-9a-f:]{17})', helper.read_text(errors='replace'))
        me = found[-1] if found else None
    owl = folder / 'owl.log'
    if owl.exists():
        text = owl.read_text(errors='replace')
        m = re.search(r'addr ([0-9a-f:]{17})\)', text)
        me = me or (m[1] if m else None)
        peers = collections.Counter(re.findall(r'add peer ([0-9a-f:]{17})', text))
        if peers:
            peer = peers.most_common(1)[0][0]
    return me, peer


def pct(values, q):
    return values[min(len(values) - 1, int(len(values) * q))] if values else 0


def radio(path, me, peer):
    acks, bas, rts, peer_data, peer_retry = [], [], 0, 0, 0
    own_addba, own_delba, awdl_addba = 0, 0, 0
    peer_slots = set()
    for t, _, data in pcap_frames(path):
        tsf, off = radiotap(data)
        w = data[off:]
        if len(w) < 10:
            continue
        when = tsf if tsf is not None else t
        fc, flags = w[0], w[1]
        kind, sub = (fc >> 2) & 3, fc >> 4
        a1 = w[4:10]
        if kind == 1:
            if a1 != me:
                continue
            if sub == 13:
                acks.append(when)
            elif sub == 9:
                bas.append(when)
            elif sub == 11:
                rts += 1
            continue
        if len(w) < 24:
            continue
        a2, a3 = w[10:16], w[16:22]
        if kind == 2 and a2 == peer and a1 == me:
            peer_data += 1
            peer_retry += bool(flags & 0x08)
            peer_slots.add(int(when / SLOT))
        elif kind == 0 and sub == 13 and a2 == me and len(w) > 25 and w[24] == 3:
            # Block Ack action frames we sent (the monitor shows our own frames on some drivers)
            if a3 == AWDL_BSSID:
                awdl_addba += w[25] == 0
            elif a3 == me:
                own_addba += w[25] == 0
                own_delba += w[25] == 2
        if a2 == peer:
            peer_slots.add(int(when / SLOT))
    return {'acks': sorted(acks), 'bas': sorted(bas), 'rts': rts, 'peer_data': peer_data, 'peer_retry': peer_retry,
            'own_addba': own_addba, 'own_delba': own_delba, 'awdl_addba': awdl_addba, 'peer_slots': peer_slots}


def bursts(times, gap=0.005):
    """Sizes of runs of acknowledgements closer than `gap`, and the silences between runs."""
    if not times:
        return [], []
    sizes, silences = [1], []
    for a, b in zip(times, times[1:]):
        if b - a > gap:
            sizes.append(1)
            silences.append(b - a)
        else:
            sizes[-1] += 1
    return sizes, silences


def tcp(path):
    """The bulk TCP flow of awdl0.pcap: bytes, seconds, RTT, in flight, receive window, retransmissions."""
    packets = []
    for t, linktype, d in pcap_frames(path):
        if linktype != 1 or len(d) < 14 + 40 + 20 or struct.unpack('>H', d[12:14])[0] != 0x86dd:
            continue
        ip = d[14:]
        if ip[6] != 6:
            continue
        seg = ip[40:]
        sport, dport, seq, ack, hlen, flags, win = struct.unpack('>HHIIBBH', seg[:16])
        hlen = (hlen >> 4) * 4
        payload = struct.unpack('>H', ip[4:6])[0] - hlen
        scale, o = None, 20
        while o < hlen and o < len(seg):
            kind = seg[o]
            if kind == 0:
                break
            if kind == 1:
                o += 1
                continue
            if o + 1 >= len(seg):
                break
            if kind == 3 and o + 2 < len(seg):
                scale = seg[o + 2]
            o += max(seg[o + 1], 2)
        packets.append((t, ip[8:24], ip[24:40], sport, dport, seq, ack, flags, win, payload, scale))
    sizes = collections.Counter()
    for p in packets:
        sizes[(p[1], p[2], p[3], p[4])] += max(p[9], 0)
    if not sizes:
        return None
    flow, total = sizes.most_common(1)[0]
    fwd = [p for p in packets if (p[1], p[2], p[3], p[4]) == flow and p[9] > 0]
    rev = [p for p in packets if (p[2], p[1], p[4], p[3]) == flow]
    if len(fwd) < 2:
        return None
    scales = [p[10] for p in rev if p[7] & 2 and p[10] is not None]
    scale = scales[0] if scales else 0
    count = collections.Counter(p[5] for p in fwd)
    ends = sorted(((p[5] + p[9]) & 0xffffffff, p[0]) for p in fwd if count[p[5]] == 1)
    ends.sort(key=lambda e: e[1])
    acks = [(p[0], p[6]) for p in sorted(rev, key=lambda p: p[0]) if p[7] & 16]
    rtts, i = [], 0
    for end, sent in ends:
        while i < len(acks) and (acks[i][0] < sent or ((acks[i][1] - end) & 0xffffffff) > 0x7fffffff):
            i += 1
        if i < len(acks):
            rtts.append(acks[i][0] - sent)
    rtts.sort()
    windows = sorted(p[8] << scale for p in rev if not p[7] & 2)
    flight, high, acked = [], None, None
    for t, kind, p in sorted([(p[0], 'd', p) for p in fwd] + [(a[0], 'a', a) for a in rev if a[7] & 16],
                             key=lambda x: x[0]):
        if kind == 'd':
            e = (p[5] + p[9]) & 0xffffffff
            if high is None or ((e - high) & 0xffffffff) < 0x7fffffff:
                high = e
        else:
            acked = p[6]
        if high is not None and acked is not None:
            flight.append((high - acked) & 0xffffffff)
    flight.sort()
    seconds = fwd[-1][0] - fwd[0][0]
    return {'bytes': total, 'seconds': seconds, 'rtt': rtts, 'window': windows, 'flight': flight,
            'retrans': sum(v - 1 for v in count.values() if v > 1), 'segments': len(count)}


def owl_log(path):
    if not path.exists():
        return {}
    text = path.read_text(errors='replace')
    return {'inject_fail': text.count('unable to inject'),
            'rate_changes': re.findall(r'unicast data rate now ([^(]+)', text),
            'rate_start': (re.findall(r'unicast data rate: ([^\n]+)', text) or [''])[0].strip(),
            'station': bool(re.search(r'station: registered|stations (without Block Ack: )?peers we send', text)),
            'ba_agreed': 'agreed; its frames are aggregated' in text,
            'ba_asked': text.count('Block Ack: asked'),
            'ba_accepted': text.count('Block Ack: accepted request')}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('folder', type=Path, help='a trace folder from scripts/airdrop-trace.sh')
    parser.add_argument('--me', help='our AWDL address (default: from iface.txt / owl.log)')
    parser.add_argument('--peer', help="the iPhone's AWDL address (default: the peer OWL saw most)")
    args = parser.parse_args()
    folder = args.folder
    me_text, peer_text = addresses(folder)
    me, peer = mac(args.me or me_text), mac(args.peer or peer_text)
    if not me or not peer:
        sys.exit('Cannot tell our address or the peer\'s: pass --me and --peer (aa:bb:cc:dd:ee:ff).')
    print(f'Trace: {folder}')
    hints = []

    owl = owl_log(folder / 'owl.log')
    if (folder / 'air.pcap').exists():
        r = radio(folder / 'air.pcap', me, peer)
        acked = r['acks'] + r['bas']
        span = (max(acked) - min(acked)) if len(acked) > 1 else 0
        sizes, silences = bursts(sorted(acked))
        long_gaps = [s for s in silences if s >= 0.05]
        slots = len({int(t / SLOT) for t in acked})
        print('\nRadio (air.pcap)')
        print(f"  our frames acknowledged: {len(r['acks'])} ACKs, {len(r['bas'])} Block Acks"
              + (f" over {span:.1f} s ({len(r['acks']) / span:.0f} ACKs/s)" if span else ''))
        if sizes:
            common = collections.Counter(sizes).most_common(3)
            print(f"  bursts: {len(sizes)}, most common sizes {', '.join(f'{n} x{c}' for n, c in common)}; "
                  f"silences >= 50 ms: {len(long_gaps)} ({sum(long_gaps):.1f} s)")
            if r['peer_slots']:
                print(f"  16 TU slots with our traffic acknowledged: {slots} of {len(r['peer_slots'])} the peer was heard in")
        if r['peer_data']:
            ratio = r['peer_retry'] / r['peer_data']
            print(f"  peer's frames to us: {r['peer_data']}, retries {100 * ratio:.0f} %; RTS to us: {r['rts']}")
        print(f"  Block Ack requests we sent: AWDL format {r['awdl_addba']}, mac80211 format {r['own_addba']}"
              f" (DELBA {r['own_delba']})")

        if not acked:
            hints.append('Nothing we sent was acknowledged. Either our frames never reach the air (a driver that '
                         'drops or rewrites the rate of injected frames, e.g. stock mt7921e sends them at 1 Mbit/s '
                         'CCK; a No-IR channel: check `iw reg get` and `airdrop doctor`), or the peer never came to '
                         'our channel.')
        elif sizes and sum(long_gaps) > 0.3 * span and statistics_median(sizes) <= 40:
            hints.append(f'Our frames leave in short bursts (typically {statistics_median(sizes)}) with long '
                         'silences while the peer is there. The driver or chip holds frames: on Realtek rtw89 this '
                         'was a peer without a station entry. Try AIRDROP_HELPER_ARGS=--tx-sta (stations without '
                         'Block Ack) and compare.')
        if r['peer_data'] > 200 and r['peer_retry'] / r['peer_data'] > 0.3:
            hints.append('The peer retries most of its frames to us: our card does not acknowledge them in time. '
                         'A card without active monitor mode needs the access point in borrow mode (the default); '
                         'on rtw89 the station entry fixed this (61 % -> 10 %).')
        if r['own_addba']:
            hints.append(f"Our driver sent {r['own_addba']} Block Ack requests in mac80211's format. The iPhone only "
                         "answers AWDL's format, and each request stops that queue for up to a second. A driver "
                         "patch can stop them for injected frames (driver/rtw89-awdl.diff does).")
        if acked and not r['bas'] and owl.get('station') and not owl.get('ba_agreed'):
            hints.append('Single frames only (no Block Acks from the peer). Around 5-6 MB/s is the ceiling for single '
                         'frames; aggregation needs a driver that turns the firmware\'s Block Ack on for the peer '
                         '(awdl_ba in our MT7921 and rtw89 builds).')
    else:
        print('\nNo air.pcap: the radio part is skipped.')

    if (folder / 'awdl0.pcap').exists():
        t = tcp(folder / 'awdl0.pcap')
        if t:
            print('\nTCP (awdl0.pcap, the bulk direction)')
            print(f"  {t['bytes'] / 1e6:.1f} MB in {t['seconds']:.1f} s = {t['bytes'] / max(t['seconds'], 1e-6) / 1e6:.2f} MB/s; "
                  f"retransmitted {t['retrans']} of {t['segments']} segments")
            if t['rtt']:
                print(f"  round trip: median {1000 * pct(t['rtt'], .5):.0f} ms, 90 % {1000 * pct(t['rtt'], .9):.0f} ms")
            if t['window'] and t['flight']:
                print(f"  receive window median {pct(t['window'], .5) // 1024} KB; in flight median "
                      f"{pct(t['flight'], .5) // 1024} KB")
            if t['rtt'] and pct(t['rtt'], .5) > 0.1:
                hints.append('TCP round trips are long (median over 100 ms): frames wait in a queue below OWL '
                             '(the socket fills, see "unable to inject" below) or the peer is rarely on our channel.')

    if owl:
        print('\nOWL (owl.log)')
        print(f"  rate: {owl['rate_start'] or '?'}" + (f"; changed {len(owl['rate_changes'])} times, last "
                                                       f"{owl['rate_changes'][-1].strip()}" if owl['rate_changes'] else ''))
        print(f"  station entry: {'yes' if owl['station'] else 'no'}; Block Ack: asked {owl['ba_asked']}, "
              f"agreed {'yes' if owl['ba_agreed'] else 'no'}; peer's requests accepted {owl['ba_accepted']}")
        print(f"  'unable to inject' (socket full, driver behind): {owl['inject_fail']}")
        if owl['inject_fail'] > 100:
            hints.append('Many "unable to inject": the driver drains our frames more slowly than OWL hands them over. '
                         'Look at the bursts above for where they wait.')

    print('\nLikely causes' if hints else '\nNothing stands out.')
    for h in hints:
        print(f'  - {h}')


def statistics_median(values):
    ordered = sorted(values)
    return ordered[len(ordered) // 2]


if __name__ == '__main__':
    main()
