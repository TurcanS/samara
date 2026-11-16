#!/usr/bin/env python3
"""Show the TLS handshakes of AirDrop connections in an awdl0 pcap.

    python3 -I scripts/tls-hello.py probe/trace/awdl0.pcap [--port 8771] [--save-certs DIR]   (this client listens on 8771; Apple on 8770)

For every TCP connection to the AirDrop port: the client's ClientHello (record and
handshake versions, cipher suites, extensions in order, ALPN, SNI, supported
groups and versions, signature algorithms), the server's ServerHello (chosen
version and cipher), and every plaintext handshake message after it. With TLS 1.2
the client's Certificate message is visible here; with TLS 1.3 everything after
ServerHello is encrypted. Also prints a JA4-like summary line per client.
Reads classic pcap with Ethernet (1), raw (101) or Linux cooked (113) link types.
"""
import collections
import hashlib
import struct
import sys

CIPHERS = {0x1301: "TLS_AES_128_GCM_SHA256", 0x1302: "TLS_AES_256_GCM_SHA384",
           0x1303: "TLS_CHACHA20_POLY1305_SHA256", 0xc02b: "ECDHE_ECDSA_AES128_GCM",
           0xc02c: "ECDHE_ECDSA_AES256_GCM", 0xc02f: "ECDHE_RSA_AES128_GCM",
           0xc030: "ECDHE_RSA_AES256_GCM", 0xcca8: "ECDHE_RSA_CHACHA20",
           0xcca9: "ECDHE_ECDSA_CHACHA20", 0x00ff: "EMPTY_RENEGOTIATION_SCSV"}
EXTS = {0: "server_name", 5: "status_request", 10: "supported_groups", 11: "ec_point_formats",
        13: "signature_algorithms", 16: "alpn", 18: "sct", 21: "padding", 22: "encrypt_then_mac",
        23: "extended_master_secret", 27: "compress_certificate", 35: "session_ticket",
        41: "pre_shared_key", 42: "early_data", 43: "supported_versions", 45: "psk_kex_modes",
        49: "post_handshake_auth", 50: "signature_algorithms_cert", 51: "key_share",
        65281: "renegotiation_info"}
GROUPS = {23: "secp256r1", 24: "secp384r1", 25: "secp521r1", 29: "x25519", 30: "x448",
          0x11ec: "X25519MLKEM768", 0x6399: "X25519Kyber768"}
HS = {1: "ClientHello", 2: "ServerHello", 4: "NewSessionTicket", 8: "EncryptedExtensions",
      11: "Certificate", 12: "ServerKeyExchange", 13: "CertificateRequest", 14: "ServerHelloDone",
      15: "CertificateVerify", 16: "ClientKeyExchange", 20: "Finished"}
VER = {0x0301: "1.0", 0x0302: "1.1", 0x0303: "1.2", 0x0304: "1.3"}


def is_grease(v):
    return (v & 0x0f0f) == 0x0a0a and (v >> 8) == (v & 0xff)


def packets(path):
    d = open(path, "rb").read()
    le = d[:4] in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1")
    e = "<" if le else ">"
    link = struct.unpack(e + "I", d[20:24])[0]
    off = 24
    while off + 16 <= len(d):
        ts, tu, incl, _ = struct.unpack(e + "IIII", d[off:off + 16])
        p = d[off + 16:off + 16 + incl]
        off += 16 + incl
        if link == 1:
            if len(p) < 14:
                continue
            et, p = struct.unpack(">H", p[12:14])[0], p[14:]
        elif link == 113:
            et, p = struct.unpack(">H", p[14:16])[0], p[16:]
        elif link == 101:
            et = 0x86dd if p and p[0] >> 4 == 6 else 0x0800
        else:
            continue
        if et == 0x86dd and len(p) >= 40 and p[6] == 6:
            src, dst, l4 = p[8:24], p[24:40], p[40:40 + struct.unpack(">H", p[4:6])[0]]
        elif et == 0x0800 and len(p) >= 20 and p[9] == 6:
            ihl = (p[0] & 15) * 4
            src, dst, l4 = p[12:16], p[16:20], p[ihl:struct.unpack(">H", p[2:4])[0]]
        else:
            continue
        if len(l4) < 20:
            continue
        sp, dp, seq = struct.unpack(">HHI", l4[:8])
        flags, hl = l4[13], (l4[12] >> 4) * 4
        yield ts + tu / 1e6, src, sp, dst, dp, seq, flags, l4[hl:]


def ip6(b):
    if len(b) == 4:
        return ".".join(map(str, b))
    import ipaddress
    return str(ipaddress.IPv6Address(b))


def streams(path, port):
    """Reassemble each direction of each connection (by sequence number)."""
    segs = collections.defaultdict(dict)
    first = {}
    for ts, src, sp, dst, dp, seq, flags, data in packets(path):
        if port not in (sp, dp):
            continue
        key = (ip6(src), sp, ip6(dst), dp)
        if flags & 0x02:                       # SYN: data starts at seq+1
            first[key] = seq + 1
            first.setdefault(("t",) + key, ts)
        if data:
            segs[key].setdefault(seq, data)
            first.setdefault(("t",) + key, ts)
    for key, s in segs.items():
        base = first.get(key, min(s))
        buf, nxt = bytearray(), base
        for seq in sorted(s):
            rel = (seq - nxt) & 0xffffffff
            if rel > 1 << 31:                  # retransmit overlapping what we have
                d = s[seq][(nxt - seq) & 0xffffffff:]
            elif rel == 0:
                d = s[seq]
            else:
                break                          # hole: stop (handshake is at the start anyway)
            buf += d
            nxt = (nxt + len(d)) & 0xffffffff
            if len(buf) > 65536:
                break
        yield first.get(("t",) + key, 0.0), key, bytes(buf)


def handshakes(buf):
    """Yield (record version, handshake type, body) for plaintext handshake messages."""
    i, hbuf, rver = 0, b"", None
    while i + 5 <= len(buf):
        ctype, ver, ln = buf[i], struct.unpack(">H", buf[i + 1:i + 3])[0], struct.unpack(">H", buf[i + 3:i + 5])[0]
        rec = buf[i + 5:i + 5 + ln]
        i += 5 + ln
        if ctype == 20:
            yield ver, "ChangeCipherSpec", b""
            continue
        if ctype == 23:
            yield ver, "ApplicationData(encrypted)", rec
            return
        if ctype == 21:
            yield ver, "Alert", rec
            continue
        if ctype != 22:
            return
        hbuf += rec
        rver = ver
        while len(hbuf) >= 4:
            t, l = hbuf[0], int.from_bytes(hbuf[1:4], "big")
            if len(hbuf) < 4 + l:
                break
            yield rver, t, hbuf[4:4 + l]
            hbuf = hbuf[4 + l:]


def parse_exts(b):
    out = []
    i = 0
    while i + 4 <= len(b):
        t, l = struct.unpack(">HH", b[i:i + 4])
        out.append((t, b[i + 4:i + 4 + l]))
        i += 4 + l
    return out


def u16list(b):
    return [struct.unpack(">H", b[i:i + 2])[0] for i in range(0, len(b) - 1, 2)]


def client_hello(b):
    ver = struct.unpack(">H", b[:2])[0]
    i = 34
    sid = b[i + 1:i + 1 + b[i]]; i += 1 + b[i]
    cl = struct.unpack(">H", b[i:i + 2])[0]; ciphers = u16list(b[i + 2:i + 2 + cl]); i += 2 + cl
    i += 1 + b[i]
    exts = parse_exts(b[i + 2:i + 2 + struct.unpack(">H", b[i:i + 2])[0]]) if i + 2 <= len(b) else []
    info = {"legacy_version": VER.get(ver, hex(ver)), "session_id_len": len(sid), "ciphers": ciphers,
            "exts": [t for t, _ in exts]}
    for t, v in exts:
        if t == 0 and len(v) > 5:
            info["sni"] = v[5:5 + struct.unpack(">H", v[3:5])[0]].decode(errors="replace")
        elif t == 16:
            names, j = [], 2
            while j < len(v):
                names.append(v[j + 1:j + 1 + v[j]].decode(errors="replace")); j += 1 + v[j]
            info["alpn"] = names
        elif t == 10:
            info["groups"] = u16list(v[2:])
        elif t == 13:
            info["sigalgs"] = u16list(v[2:])
        elif t == 43:
            info["versions"] = u16list(v[1:])
        elif t == 51:
            ks, j = [], 2
            while j + 4 <= len(v):
                g, l = struct.unpack(">HH", v[j:j + 4]); ks.append((g, l)); j += 4 + l
            info["key_share"] = ks
    return info


def server_hello(b):
    ver = struct.unpack(">H", b[:2])[0]
    i = 34
    i += 1 + b[i]
    cipher = struct.unpack(">H", b[i:i + 2])[0]
    i += 3
    group = None
    exts = parse_exts(b[i + 2:i + 2 + struct.unpack(">H", b[i:i + 2])[0]]) if i + 2 <= len(b) else []
    for t, v in exts:
        if t == 43 and len(v) == 2:
            ver = struct.unpack(">H", v)[0]
        if t == 51 and len(v) >= 2:
            group = struct.unpack(">H", v[:2])[0]
    return {"version": VER.get(ver, hex(ver)), "cipher": cipher, "exts": [t for t, _ in exts],
            "group": group}


def name(table, v):
    return "GREASE" if is_grease(v) else table.get(v, f"{v:#06x}")


def ja4ish(ch):
    c = [x for x in ch["ciphers"] if not is_grease(x)]
    e = [x for x in ch["exts"] if not is_grease(x)]
    v = max([x for x in ch.get("versions", []) if not is_grease(x)] or [0x0303])
    head = f"t{VER.get(v, '??').replace('.', '')}{'d' if 'sni' in ch else 'i'}{len(c):02d}{len(e):02d}" \
           f"{(ch.get('alpn') or ['00'])[0][:2]}"
    hc = hashlib.sha256(",".join(f"{x:04x}" for x in sorted(c)).encode()).hexdigest()[:12]
    he = hashlib.sha256((",".join(f"{x:04x}" for x in sorted(x for x in e if x not in (0, 16))) + "_" +
                         ",".join(f"{x:04x}" for x in ch.get("sigalgs", []))).encode()).hexdigest()[:12]
    return f"{head}_{hc}_{he}"


def main():
    path = sys.argv[1]
    port = int(sys.argv[sys.argv.index("--port") + 1]) if "--port" in sys.argv else 8771
    save_dir = sys.argv[sys.argv.index("--save-certs") + 1] if "--save-certs" in sys.argv else None
    conns = collections.defaultdict(dict)
    for ts, (s, sp, d, dp), buf in streams(path, port):
        role = "client" if dp == port else "server"
        cid = (s, sp, d, dp) if role == "client" else (d, dp, s, sp)
        conns[cid][role] = buf
        conns[cid].setdefault("t", ts)
    fingerprints = collections.Counter()
    for (cli, cport, srv, sport), c in sorted(conns.items(), key=lambda kv: kv[1]["t"]):
        print(f"\n== {c['t']:.3f} client [{cli}]:{cport} -> server [{srv}]:{sport}")
        for role in ("client", "server"):
            seen = []
            for rver, t, body in handshakes(c.get(role, b"")):
                label = HS.get(t, t) if isinstance(t, int) else t
                seen.append(label)
                if t == 1:
                    ch = client_hello(body)
                    fingerprints[(cli, ja4ish(ch))] += 1
                    print(f"   ClientHello  record {VER.get(rver, hex(rver))}  legacy {ch['legacy_version']}  "
                          f"session-id {ch['session_id_len']} bytes")
                    print(f"     versions : {[name(VER, v) for v in ch.get('versions', [])]}")
                    print(f"     ciphers  : {[name(CIPHERS, x) for x in ch['ciphers']]}")
                    print(f"     exts     : {[name(EXTS, x) for x in ch['exts']]}")
                    print(f"     alpn {ch.get('alpn')}  sni {ch.get('sni')!r}")
                    print(f"     groups   : {[name(GROUPS, g) for g in ch.get('groups', [])]}")
                    print(f"     key_share: {[(name(GROUPS, g), l) for g, l in ch.get('key_share', [])]}")
                    print(f"     sigalgs  : {[f'{x:#06x}' for x in ch.get('sigalgs', [])]}")
                    print(f"     JA4-like : {ja4ish(ch)}")
                elif t == 2:
                    sh = server_hello(body)
                    print(f"   ServerHello  {sh['version']}  cipher {name(CIPHERS, sh['cipher'])}  "
                          f"group {name(GROUPS, sh['group']) if sh['group'] else None}  exts {[name(EXTS, x) for x in sh['exts']]}")
                elif t == 11:
                    n = int.from_bytes(body[:3], "big") if body else 0
                    print(f"   {role} Certificate message, {n} bytes of certificates (plaintext => TLS 1.2)")
                    j, k = 3, 0
                    while j + 3 <= 3 + n:
                        cl = int.from_bytes(body[j:j + 3], "big")
                        der = body[j + 3:j + 3 + cl]
                        j += 3 + cl
                        print(f"     cert {k}: {cl} bytes, sha256 {hashlib.sha256(der).hexdigest()[:16]}")
                        if save_dir:
                            import os
                            os.makedirs(save_dir, exist_ok=True)
                            out = os.path.join(save_dir, f"{role}-{cport}-cert{k}.der")
                            open(out, "wb").write(der)
                            print(f"       saved {out}")
                        k += 1
            print(f"   {role} messages: {seen or '(none captured)'}")
    print("\n== client fingerprints")
    for (cli, fp), n in fingerprints.most_common():
        print(f"   {n:3d} x {fp}  from {cli}")


if __name__ == "__main__":
    main()
