#!/usr/bin/env python3
"""Decode Apple AirDrop (Continuity type 0x05) BLE adverts from `btmon` text output.

    btmon > ble.txt            (or: btmon -w ble.btsnoop; btmon -r ble.btsnoop > ble.txt)
    python3 -I scripts/ble-airdrop.py ble.txt [--all]
    python3 -I scripts/ble-airdrop.py ble.txt --types   (every Apple Continuity type per address;
                                                        flags 10-byte `01 …` items = WPAWDL, S-18/E1.5)

Per advertiser address: first/last time seen, PDU types (connectable or not), and
every distinct 18-byte payload split into the fields we know about:

    40 | d6 91 ba | 00 00 00 00 | 03 | aa aa | bb bb | dd dd | cc cc | 00
    b0   tag        reserved      ver  hash1   hash2   hash3   hash4   end

It also checks a few hypotheses about the 3-byte tag (does it equal bytes of
the resolvable address or a SHA-256 prefix of it, does it change only when the
address rotates) so they can be confirmed or refuted against new captures.
Only reads the text file; needs no privileges.
"""
import collections
import hashlib
import re
import sys

HDR = re.compile(r"^[<>@=].*?(\d+\.\d+)\s*$")
ADDR = re.compile(r"^\s+(?:LE )?Address: ([0-9A-F:]{17})(?: \((\w+)\))?")
PDU = re.compile(r"Legacy PDU Type: (\w+)")
HEX = re.compile(r"^\s+((?:[0-9a-f]{2} )+[0-9a-f]{2})\s")


def adverts(path):
    """Yield (time, address, address type, pdu type, 18-byte payload)."""
    t, addr, atype, pdu, want = 0.0, None, None, None, 0
    buf = b""
    for raw in open(path, errors="replace"):
        line = re.sub(r"\x1b\[[0-9;]*m", "", raw.rstrip("\n"))
        if m := HDR.match(line):
            t, addr, atype, pdu = float(m.group(1)), None, None, None
            continue
        if m := ADDR.match(line):
            if not line.strip().startswith("Direct"):
                addr, atype = m.group(1), m.group(2)
            continue
        if m := PDU.search(line):
            pdu = m.group(1)
            continue
        if "Type: AirDrop (5)" in line:
            want, buf = 18, b""
            continue
        if want and (m := HEX.match(line)):
            buf += bytes.fromhex(m.group(1).replace(" ", ""))
            if len(buf) >= want:
                yield t, addr, atype, pdu, buf[:want]
                want = 0


TYPE = re.compile(r"^\s+Type: (.+?) \((\d+)\)\s*$")
DATA = re.compile(r"^\s+Data\[(\d+)\]:")


def continuity_items(path):
    """Yield (time, address, type number, type name, data bytes) for every Apple
    Continuity item btmon prints (named or "Unknown")."""
    t, addr, cur, want, buf = 0.0, None, None, 0, b""
    for raw in open(path, errors="replace"):
        line = re.sub(r"\x1b\[[0-9;]*m", "", raw.rstrip("\n"))
        if m := HDR.match(line):
            t, addr, cur, want = float(m.group(1)), None, None, 0
            continue
        if m := ADDR.match(line):
            if not line.strip().startswith("Direct"):
                addr = m.group(1)
            continue
        if m := TYPE.match(line):
            cur, want, buf = (int(m.group(2)), m.group(1)), 0, b""
            if cur[0] == 5:
                want = 18
            continue
        if cur and (m := DATA.match(line)):
            want, buf = int(m.group(1)), b""
            continue
        if cur and want and (m := HEX.match(line)):
            buf += bytes.fromhex(m.group(1).replace(" ", ""))
            if len(buf) >= want:
                yield t, addr, cur[0], cur[1], buf[:want]
                cur, want = None, 0


def types_report(path):
    tally = collections.Counter()
    wpawdl = collections.Counter()
    for t, addr, num, name, data in continuity_items(path):
        tally[(addr, num, name, len(data))] += 1
        if len(data) == 10 and data[0] == 0x01:
            wpawdl[(addr, data.hex(" "))] += 1
    for (addr, num, name, ln), c in sorted(tally.items(), key=lambda kv: (kv[0][0] or "", kv[0][1])):
        print(f"{addr}  type {num:3} ({name}) len {ln:3}  x{c}")
    print("\n== 10-byte items starting 01 (WPAWDL candidates)")
    for (addr, hx), c in wpawdl.most_common():
        print(f"   {addr}  {hx}  x{c}")
    if not wpawdl:
        print("   none")


def fields(p):
    return {"b0": f"{p[0]:02x}", "tag": p[1:4].hex(), "reserved": p[4:8].hex(),
            "version": p[8], "hashes": " ".join(p[9 + 2 * i:11 + 2 * i].hex() for i in range(4)),
            "end": f"{p[17]:02x}"}


def tag_hypotheses(addr, tag):
    a = bytes.fromhex(addr.replace(":", ""))
    out = []
    for name, src in (("addr (MSB first)", a), ("addr (LSB first)", a[::-1])):
        if tag in src.hex():
            out.append(f"tag found inside {name}")
        if hashlib.sha256(src).digest()[:3].hex() == tag:
            out.append(f"tag == sha256({name})[:3]")
    return out or ["tag not derived from the address by any tested rule"]


def main():
    path, show_all = sys.argv[1], "--all" in sys.argv
    if "--types" in sys.argv:
        return types_report(path)
    by_addr = collections.OrderedDict()
    order = []
    for t, addr, atype, pdu, p in adverts(path):
        n = by_addr.setdefault(addr, {"first": t, "last": t, "type": atype, "count": 0,
                                      "pdu": collections.Counter(), "payloads": collections.Counter()})
        n["last"], n["count"] = t, n["count"] + 1
        if pdu:
            n["pdu"][pdu] += 1
        n["payloads"][p] += 1
        if not order or order[-1][1] != p:
            order.append((t, p, addr))
        if show_all:
            f = fields(p)
            print(f"{t:10.3f} {addr} {pdu or '-':16} b0={f['b0']} tag={f['tag']} v{f['version']} "
                  f"hashes={f['hashes']}")
    if not by_addr:
        print("no AirDrop (type 5) adverts found")
        return
    for addr, n in by_addr.items():
        print(f"\n== {addr} ({n['type']}): {n['count']} adverts, {n['first']:.3f}-{n['last']:.3f} s")
        print(f"   PDU types: {dict(n['pdu']) or 'only MGMT Device Found (no PDU type)'}")
        for p, c in n["payloads"].most_common():
            f = fields(p)
            print(f"   x{c:<4} b0={f['b0']} tag={f['tag']} reserved={f['reserved']} "
                  f"version={f['version']} hashes={f['hashes']} end={f['end']}")
            for h in tag_hypotheses(addr, f["tag"]):
                print(f"          {h}")
    print("\n== payload changes over time")
    for t, p, addr in order:
        f = fields(p)
        print(f"   {t:10.3f} {addr} tag={f['tag']} b0={f['b0']} hashes={f['hashes']}")


if __name__ == "__main__":
    main()
