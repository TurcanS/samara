#!/usr/bin/env python3
"""Compare `airdrop receive --dump` sessions: which keys each request carried.

    python3 -I scripts/dump-diff.py probe/sessions/*/dump/*   [--values]

One column per dump session (labelled by the session folder name, e.g. the
NAME given to airdrop-trace.sh), one row per key path, for /Discover, /Ask,
/Upload headers, unknown endpoints and TLS. A cell shows the value type, or
the value itself with --values (long values are shortened). Use it to see
what a link share, a contact card or a video adds over a photo.
"""
import collections
import json
import sys
from pathlib import Path

SKIP = {"time", "peer"}


def flatten(obj, prefix=""):
    if isinstance(obj, dict):
        if set(obj) <= {"bytes", "hex", "file", "head_hex"} and "bytes" in obj:
            yield prefix, f"<{obj['bytes']} bytes>"
            return
        for k, v in obj.items():
            yield from flatten(v, f"{prefix}.{k}" if prefix else k)
    elif isinstance(obj, list):
        if not obj:
            yield prefix + "[]", "[]"
        for v in obj:
            yield from flatten(v, prefix + "[]")
    else:
        yield prefix, obj


def label(session: Path) -> str:
    # probe/sessions/<time>-<NAME>/dump/<time> -> <NAME>; else the folder name
    up = session.parent.parent.name if session.parent.name == "dump" else session.name
    return up.split("-", 2)[-1] if up.count("-") >= 2 else up


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    show = "--values" in sys.argv
    cols, table = [], collections.defaultdict(dict)
    for s in map(Path, args):
        if not s.is_dir():
            continue
        col = label(s)
        cols.append(col)
        for f in sorted(s.glob("*.json")):
            rec = json.loads(f.read_text())
            kind = rec.get("request", "TLS" if f.name.startswith("tls-") else f.stem).split(" ")[-1]
            for key, val in flatten({k: v for k, v in rec.items() if k not in SKIP}):
                cell = repr(val)[:40] if show else type(val).__name__
                row = (kind, key)
                prev = table[row].get(col)
                table[row][col] = cell if prev in (None, cell) else f"{prev}|{cell}"[:60]
    if not cols:
        print("no dump sessions given")
        return
    w = max(len(f"{k} {p}") for k, p in table) + 2
    print("".ljust(w) + "".join(c[:24].ljust(26) for c in cols))
    for (kind, key) in sorted(table):
        r = table[(kind, key)]
        print(f"{kind} {key}".ljust(w) + "".join(str(r.get(c, "-"))[:24].ljust(26) for c in cols))


if __name__ == "__main__":
    main()
