#!/usr/bin/env python3
"""Decode AWDL action frames from a radiotap pcap (e.g. probe/trace/air.pcap).

    python3 -I scripts/awdl-decode.py probe/trace/air.pcap [--all]

For each AWDL node: frame counts, the TLV types it sends, and decoded values of
the TLVs whose layout is known (owly's docs/SPEC.md §3.2, github.com/t4t5/omdrop-owl): channel
sequence, version + device class, data path state (country, social channels),
ARPA host name, election v2, sync parameters. Undocumented TLVs (e.g. 0x21 from
iOS 27) are printed as hex so they can be compared across devices and versions.
By default each distinct value is shown once per node; --all prints every frame.
"""
import collections
import struct
import sys

NAMES = {2: "service-response", 4: "sync-params", 5: "election", 6: "service-params",
         7: "ht-caps", 12: "data-path-state", 16: "arpa", 17: "ieee80211-container",
         18: "channel-sequence", 20: "sync-tree", 21: "version", 22: "bloom-filter",
         24: "election-v2"}
DEVCLASS = {1: "macOS", 2: "iOS", 8: "tvOS"}


def frames(path):
    d = open(path, "rb").read()
    off = 24
    while off + 16 <= len(d):
        ts, tu, incl, _ = struct.unpack("<IIII", d[off:off + 16])
        p = d[off + 16:off + 16 + incl]
        off += 16 + incl
        if len(p) < 8:
            continue
        w = p[struct.unpack("<H", p[2:4])[0]:]
        if len(w) > 40 and w[0] == 0xD0 and w[24:28] == b"\x7f\x00\x17\xf2" and w[28] == 8:
            yield ts + tu / 1e6, w


def chanseq(v):
    if len(v) < 6:
        return "?"
    count, enc = v[0] + 1, v[1]
    size = {0: 1, 1: 2, 3: 2}.get(enc, 1)
    out = []
    for i in range(count):
        c = v[6 + i * size:6 + (i + 1) * size]
        if not c:
            break
        out.append(c[1] if enc == 1 else c[0])
    return f"enc={enc} " + ",".join(map(str, out))


def decode(t, v):
    if t == 18:
        return chanseq(v)
    if t == 21 and len(v) >= 2:
        return f"AWDL {v[0] >> 4}.{v[0] & 15}, device class {DEVCLASS.get(v[1], v[1])}"
    if t == 16 and len(v) >= 2:
        n = v[1]
        return f"host '{v[2:2 + n].decode(errors='replace')}'"
    if t == 24 and len(v) >= 40:
        master, sync = v[0:6].hex(":"), v[6:12].hex(":")
        mctr, dist, mmet, own = struct.unpack("<IIII", v[12:28])
        return f"master {master} sync {sync} counter {mctr} distance {dist} master-metric {mmet} own-metric {own}"
    if t == 4 and len(v) >= 31:
        return (f"next-AW-chan {v[0]} master-chan {v[3]} AW-period {struct.unpack('<H', v[5:7])[0]} "
                f"master {v[21:27].hex(':')} next-AW-seq {struct.unpack('<H', v[29:31])[0]}")
    if t == 12 and len(v) >= 2:
        flags = struct.unpack("<H", v[0:2])[0]
        out, i = [f"flags {flags:#06x}"], 2
        if flags & 0x0100 and len(v) >= i + 3:
            out.append(f"country {v[i:i + 3]!r}"); i += 3
        if flags & 0x0200 and len(v) >= i + 2:
            m = struct.unpack("<H", v[i:i + 2])[0]
            out.append("social " + "/".join(c for b, c in ((1, "6"), (2, "44"), (4, "149")) if m & b)); i += 2
        if flags & 0x0001 and len(v) >= i + 8:
            out.append(f"infra {v[i:i + 6].hex(':')} ch {v[i + 6]}"); i += 8
        return " ".join(out)
    return v.hex()


def main():
    path = sys.argv[1]
    show_all = "--all" in sys.argv
    if "--tlv" in sys.argv:
        return tlv_timeline(path, int(sys.argv[sys.argv.index("--tlv") + 1], 0))
    nodes = collections.defaultdict(lambda: {"frames": 0, "sub": collections.Counter(),
                                             "seen": collections.defaultdict(set)})
    for ts, w in frames(path):
        src = w[10:16].hex(":")
        n = nodes[src]
        n["frames"] += 1
        n["sub"][{0: "PSF", 3: "MIF"}.get(w[30], w[30])] += 1
        body, i = w[40:], 0
        while i + 3 <= len(body):
            t = body[i]
            ln = struct.unpack("<H", body[i + 1:i + 3])[0]
            v = body[i + 3:i + 3 + ln]
            i += 3 + ln
            text = decode(t, v)
            if show_all or text not in n["seen"][t]:
                n["seen"][t].add(text)
                if show_all:
                    print(f"{ts:.3f} {src} {t:#04x} {NAMES.get(t, '?')}: {text}")
    for src, n in nodes.items():
        print(f"\n== {src}: {n['frames']} action frames {dict(n['sub'])}")
        for t in sorted(n["seen"]):
            vals = sorted(n["seen"][t])
            name = NAMES.get(t, "UNDOCUMENTED")
            print(f"  {t:#04x} {name} ({len(vals)} distinct)")
            for val in vals[:6]:
                print(f"      {val[:160]}")


def tlv_timeline(path, want):
    """--tlv N: every value of TLV N per frame, beside the same frame's channel
    sequence and data path state, collapsed into runs of identical rows."""
    rows = []
    for ts, w in frames(path):
        src, tl = w[10:16].hex(":"), {}
        body, i = w[40:], 0
        while i + 3 <= len(body):
            t, ln = body[i], struct.unpack("<H", body[i + 1:i + 3])[0]
            tl[t] = body[i + 3:i + 3 + ln]
            i += 3 + ln
        if want in tl:
            rows.append((ts, src, tl[want].hex(" "), decode(18, tl[18]) if 18 in tl else "-",
                         decode(12, tl[12]) if 12 in tl else "-"))
    print(f"TLV {want:#04x}: {len(rows)} frames  (time | sender | value | channel sequence | data path state)")
    last, n, start = None, 0, 0.0
    for r in rows + [None]:
        key = r and r[1:]
        if key != last:
            if last:
                print(f"  x{n:<5} {start:.3f}  {last[0]}  {last[1]}\n"
                      f"          seq: {last[2]}\n          dps: {last[3]}")
            last, n, start = key, 0, r[0] if r else 0
        n += 1


if __name__ == "__main__":
    main()
