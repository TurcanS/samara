# Contributing

## Hardware reports

This has run on two cards (MediaTek MT7921, Realtek RTL8852BE) and one phone, so a report from other hardware
is the most useful thing you can send, whether it worked or not. If it works but is slow,
[Faster on your card](docs/wiki/Faster-on-your-card.md) shows how to find out why. Please include:

- the Wi-Fi card (`lspci -nn` or `lsusb` line) and kernel version (`uname -r`, without a host name);
- the output of `airdrop doctor`;
- the Apple device model and OS version, and whether it was joined to Wi-Fi (2.4 or 5 GHz);
- what you ran, what you expected, and the last 30 lines of `/run/airdrop-helper.log` and `/run/airdrop-owl.log`.
- for a slow transfer: the text output of `scripts/air-check.py` on a trace of it (addresses replaced).

## Never attach raw captures

A session folder from `scripts/airdrop-trace.sh` holds packet captures and request dumps. They contain MAC
addresses, device and network names, Bluetooth contact hashes (short hashes of a phone number and email
addresses) and Apple-signed identity records, yours and those of anyone nearby. Do not upload them. Send the
text output of `scripts/analyze-trace.py` and `scripts/air-check.py` instead, and replace addresses and names.

## Scope

Samara implements AirDrop's "Everyone" mode. Work on Contacts Only, on how identity tags, keys or records are
derived or verified, on bypassing a receiver's consent, or on fuzzing Apple devices is out of scope and will not
be merged. If you find something that looks like a vulnerability in an Apple product, report it to Apple.

Protocol facts in `docs/PROTOCOL.md` are tagged **[M]** measured, **[S]** static, **[H]** hypothesis. A change
to that file needs the tag and, for [M], the command that reproduces it.

## Code

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release && python -m pytest -q
```

Tests are end-to-end: they drive the built CLI the way a user would, over loopback. Add a test of that kind for
a behaviour change, not unit tests of internals.

The root helper must keep its teardown fail-safe: nothing may leave Wi-Fi down after a session, a crash or a
kill. `docs/HELPER.md` is the contract.
