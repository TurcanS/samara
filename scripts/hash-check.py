#!/usr/bin/env python3
"""Which of your identifiers (and which normalization) produce the AirDrop contact hashes?

    scripts/hash-check.py RECORD.der [PREFIX ...]

RECORD.der is a SenderRecordData blob from a dump (`airdrop receive --dump`). PREFIX are the
2-byte hex short hashes from the BLE advert (scripts/ble-airdrop.py). You type candidate
emails / phone numbers at a hidden prompt (empty line ends). Nothing is printed back or
stored: the output names only the candidate's position, the normalization rule and what
it matched. Answers owly open question 1 (normalization) and "what is the 4th BLE hash".
"""
from __future__ import annotations

import getpass
import hashlib
import plistlib
import re
import subprocess
import sys


def record_hashes(path: str) -> dict[str, str]:
    out = subprocess.run(["openssl", "cms", "-verify", "-noverify", "-inform", "DER", "-in", path],
                         capture_output=True, check=True).stdout
    d = plistlib.loads(out)
    m = {}
    for i, h in enumerate(d.get("ValidatedEmailHashes", []), 1):
        m[h.lower()] = f"record email hash #{i}"
    for i, h in enumerate(d.get("ValidatedPhoneHashes", []), 1):
        m[h.lower()] = f"record phone hash #{i}"
    return m


def variants(text: str):
    t = text.strip()
    yield "as typed", t
    if "@" in t:
        yield "lowercase", t.lower()
        local, _, dom = t.lower().partition("@")
        yield "lowercase, local part without dots", local.replace(".", "") + "@" + dom
        yield "mailto: + lowercase", "mailto:" + t.lower()
        return
    digits = re.sub(r"\D", "", t)
    yield "digits only", digits
    yield "+digits (E.164)", "+" + digits
    yield "tel:+digits", "tel:+" + digits
    for cc in (1, 2, 3):  # drop a country code of 1-3 digits
        if len(digits) > cc + 6:
            nat = digits[cc:]
            yield f"national (drop {cc}-digit country code)", nat
            yield f"national with trunk 0 (drop {cc}-digit country code)", "0" + nat


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    rec = record_hashes(sys.argv[1])
    prefixes = {p.lower() for p in sys.argv[2:]}
    print(f"record: {len(rec)} hashes ({', '.join(h[:4] for h in rec)}); advert prefixes: "
          f"{', '.join(sorted(prefixes)) or '-'}")
    print("Type one email or phone number per line (hidden). Empty line ends.")
    n = 0
    while True:
        try:
            cand = getpass.getpass(f"candidate {n + 1}: ")
        except (KeyboardInterrupt, EOFError):
            print()
            break
        if not cand.strip():
            break
        n += 1
        hits = 0
        for rule, value in variants(cand):
            for algo in ("sha256",):
                h = hashlib.new(algo, value.encode()).hexdigest()
                if h in rec:
                    print(f"  candidate {n}: {rule} / {algo} = {rec[h]} (full match)")
                    hits += 1
                elif h[:4] in prefixes:
                    print(f"  candidate {n}: {rule} / {algo} -> prefix {h[:4]} (advert only, not in record)")
                    hits += 1
        if not hits:
            print(f"  candidate {n}: no match in any normalization")
    return 0


if __name__ == "__main__":
    sys.exit(main())
