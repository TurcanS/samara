# Findings

What two weeks of measuring AirDrop between one Linux laptop (MediaTek MT7921, one radio) and one iPhone 17
(iOS 27.2) turned up, and what a second laptop (Realtek RTL8852BE) added (findings 14-16). Each entry names the entry in [`EXPERIMENTS.md`](EXPERIMENTS.md) that holds the raw result
(search for the quoted heading) and says how to repeat it.

Everything here was measured on those devices. Treat it as a set of leads for other hardware, not as general
truths; [`wiki/Faster-on-your-card.md`](wiki/Faster-on-your-card.md) turns them into a checklist.

## How to repeat a measurement

```sh
NAME=rx   DUR=120 scripts/airdrop-trace.sh --mode borrow                          # the iPhone sends to you
NAME=send DUR=120 scripts/airdrop-trace.sh send --mode borrow -r "device name" FILE
python3 -I scripts/analyze-trace.py probe/sessions/<folder>
python3 -I scripts/air-check.py probe/sessions/<folder>       # what holds the card back, with the likely cause
```

A trace needs sudo for the packet capture and writes one folder per run: client log, OWL log, the air capture,
the frames we sent, Bluetooth, and every request as JSON. `analyze-trace.py` prints the outcome, the speed, the
share of time data flowed, the spacing of frames and how many of ours went unacknowledged. `awdl-decode.py`,
`ble-airdrop.py`, `tls-hello.py` and `dump-diff.py` read the same folder layer by layer.

Keep those folders to yourself. They hold addresses, device names and signed identity records.

Experiment switches of the root helper are passed through the environment, for example
`AIRDROP_HELPER_ARGS="--no-scan-guard" scripts/airdrop-trace.sh --mode borrow`. `docs/HELPER.md` lists them.

## The radio

### 1. One radio is enough, but only if it lets go of your Wi-Fi

A second interface on channel 149 next to a connected station looks fine and hears nothing: with an iPhone
advertising AirDrop for 114 s, the capture held 0 AWDL frames. The same hidden access point *alone*, with the
station disconnected, receives. That is what borrow mode is: pause Wi-Fi, hold channel 149 with our own access
point, run AWDL through monitor interfaces beside it, reconnect afterwards.

Entry: "E0.3b" (refuted), "E0.11 run 3" and "run 4" (first transfers). Repeat: `airdrop receive --mode shared`
on a network that is not on channel 149, against `--mode borrow`.

### 2. Sit on the iPhone's busiest channel, not on the "social" one

An iPhone joined to 5 GHz Wi-Fi spends most AWDL slots on channel 149. On channel 6 an 11.5 MB photo reset four
times; on 149 it finished in 17.6 s. One radio cannot hop per slot on this card, so picking the channel is the
whole game.

Entry: "E0.11 run 8". Repeat: `--channel 6` against the default.

### 3. The stock driver transmits injected frames at a rate that does not exist

`mt7921e` drops the requested rate of an injected unicast frame and falls back to 1 Mbit/s. That rate is not
defined on 5 GHz, so the frame never leaves the card and nothing reports an error. Multicast still goes out,
which makes the peer look half alive.

Entry: retro row R-03 and `docs/RESEARCH.md` §3. Repeat: `scripts/mt7921-ratefix.sh revert`, then a trace;
`analyze-trace.py` shows no acknowledged unicast.

### 4. A regulatory domain of `00` silently eats every frame

After a reboot the card sat in the world domain, where every 5 GHz channel is listen-only. The kernel then drops
injected frames without a word. `airdrop doctor` checks this now.

Entry: `docs/RESEARCH.md` §4. Repeat: `iw reg get`, then `airdrop doctor`.

### 5. We were deaf 55 % of the time, and it was our own Wi-Fi stack

Transfers ran in bursts of about 108 ms followed by 135 ms of silence. Frames from other channels showed up in
the capture only inside the silences, in sweep order, repeating every 9 s: with the station disconnected,
something scans back to back and drags the radio off channel 149. Aborting scans as they start (the scan guard)
took send from 1.9 to 5.6 MB/s and receive from 5.0 to 12.2 MB/s.

Entry: "P2.7 Why are we silent 70 % of a transfer?" and its two live results. Repeat: helper switch `--no-scan-guard`.
The lesson that generalises: before blaming the peer for a silence, look at what else is in your own capture.

### 6. Receive: answer the Block Ack request

The iPhone asks for a Block Ack session every 1-2 s during an upload. Unanswered, it sends every frame alone, at
about 0.30 ms per 1500 bytes regardless of the PHY rate. Answered, it aggregates: 3.2 MB/s at once, and 21 MB/s
after the access point moved to 80 MHz and we advertised VHT capabilities.

Entry: "P2.3", "P2.2 / P2.3 live results", "E-VHT80". Repeat: helper switches `--no-ampdu`, `--ht40`.

### 7. Send: 5.4 → 30.6 MB/s by making the iPhone a client of our access point

Frames injected through a monitor interface go out one per transmission. The firmware aggregates only for
stations it knows. So OWL registers the receiving iPhone as a station of the hidden access point, asks it for a
Block Ack session in AWDL's own frame format (mac80211's request is never answered), and the patched driver
switches the firmware's aggregation on when the iPhone agrees. With HT/VHT elements in the access point's beacon
the firmware then uses 80 MHz: 417 MB in 13.6 s.

Three dead ends on the way are logged too: letting mac80211 negotiate, asking before data flows (4 of 4
unanswered; 3 of 3 granted during data), and forcing the width per frame.

Entry: the six "E-TXAGG" entries in order. Repeat: `airdrop send` with and without the patched driver loaded.

### 8. A number we had wrong

For a week the log said the iPhone gave us "2-4 % of the airtime" and that this was the ceiling. That figure
divided bytes by the PHY rate. Measured on air it was 16-20 %, and the real bound was per-frame overhead. The
wrong entries are still in the log, marked as corrected.

Entry: "P2.3" (the correction), "E0.11 run 9" (the original claim).

## The protocol

### 9. The iPhone holds back the last chunk until you answer

On iOS 26 and later an `/Upload` is chunked with no `Content-Length`, and the final empty chunk is not sent until
the receiver replies. A receiver that waits for the end of the body waits forever. Reply when the archive's
`TRAILER!!!` entry has been read.

Entry: `docs/PROTOCOL.md` §10.1; covered by a loopback test. Repeat: `python -m pytest -q -k ios26`.

### 10. Whether the iPhone sees you depends on what its AWDL was doing when you appeared

The action-frame period in the iPhone's sync parameters (TLV 0x04, bytes 7-8) is 440 TU while its AWDL is idle
and 110 TU once it is active. A receiver that first appears during 440 TU, or within 2-3 s of the share sheet
opening, was found in 8 of 8 sessions. One that appears later was ignored in 14 of 14: the iPhone acknowledges
our frames, sometimes even queries for us, and never takes the answers.

This is an open problem, not a solved one. Until it is fixed: start `airdrop receive` first, then open the share
sheet.

Entry: "C-2", "C-3". Repeat: start `receive` 10 s after opening the share sheet; decode the field with
`scripts/awdl-decode.py`.

### 11. A stable address makes the iPhone stop offering Block Ack

Keeping one AWDL address across sessions looked like a tidy idea. After about seven sessions on it the iPhone
sent no Block Ack request at all and receive fell to a fraction of its speed. The address is random per session
again.

Entry: "C-3", last part.

### 12. The four hashes in the Bluetooth advert

The AirDrop advert carries four 2-byte hashes in an order that changes. Three are the first bytes of SHA-256 of
the phone number and email addresses in the sender's signed record: emails in lower case, phone numbers as
digits only, without the `+`. The fourth matched nothing its owner could name. Two bytes are short enough to
shortlist phone numbers, which is the PrivateDrop observation; nothing here goes further than that.

Entry: "E1.4". Repeat on your own phone: `scripts/ble-airdrop.py` on a `btmon` log, then
`scripts/hash-check.py`, which asks for candidates at a hidden prompt and stores nothing.

### 13. Starting a session takes 1.5 s, not 5.7

Most of a start was waiting for things the scan guard had already made unnecessary.

Entry: "Bring-up latency".

## A second card: Realtek RTL8852BE (`rtw89`, kernel 7.3.5)

### 14. Every MediaTek assumption made the Realtek slow

The same code sent at 0.2 MB/s on the second laptop. The helper chose its fast rates and aggregation by checking
for the patched `mt7921e`, so the Realtek fell back to the legacy ladder (54 Mbit/s). The stock `rtw89` already
honours injected HT and VHT rates, so the rate now depends on the driver family, not on one module. The card also
has no "active monitor" mode; in borrow mode that does not matter, because the hidden access point carries the
same address and the chip acknowledges for it.

Fixing the rate was not enough: the speed stayed at 0.2 MB/s at every rate. Ruled out one by one: Bluetooth
coexistence (Wi-Fi owns the antenna on 5 GHz), scans, power saving, mac80211's airtime limiter (AQL), and the
chip's busy-channel check (EDCCA).

Entry: "E-RTW89 Phase 0" (the earlier runs are listed in it).

### 15. The chip holds frames for a peer it has no station entry for

On air, the card sent exactly 17 frames, the iPhone acknowledged them within 2 ms, then nothing for about 140 ms,
seven times a second. A frame waited a median 424 ms between mac80211 and its status, mac80211's own queues were
empty, and the chip's DMA indexes showed it had fetched every frame. Injected frames to a peer without a station
entry go out under the access point's own entry (`rtw89_core_tx_get_mac_id`), and the chip serves that entry in
bursts. With the iPhone registered as a station of the access point (OWL `-g`: station entry, no Block Ack request,
OWL's rate kept), the same send ran at 5.5 MB/s (MCS 15 / 40 MHz throughout, nothing lost) and receive went from
0.56 to 20.5 MB/s; the iPhone's retries to us fell from 58 % to 10 %. This is now the default for every rtw89
session, stock driver included.

Entry: "E-RTW89 Phase 0". Repeat: `AIRDROP_HELPER_ARGS=--tx-sta` against a build without the rtw89 default.

### 16. Aggregation on rtw89: 5.5 → 40.5 MB/s

The MT7921 recipe (finding 7) carries over as a patch to `rtw89_core` (`driver/rtw89-awdl.diff`, module
parameter `awdl_ba`): an AWDL-format ADDBA response from a station switches the firmware's aggregation on, and
injected frames no longer make the driver start mac80211's own Block Ack session (the trace with the station
entry had six such requests, each one stopping the queue for up to a second). 417 MB went out in 10.3 s at
VHT MCS 9, 80 MHz, two streams, without a single retry; receive reached 25.2 MB/s. That is faster than the MT7921
in both directions.

Entry: "E-RTW89 F1". Repeat: `scripts/rtw89-build.sh`, `scripts/rtw89-load.sh load`, `airdrop send`.

## What is not here

Contacts Only mode, and anything about how identity tags, keys or records are derived or verified. This project
implements Everyone mode and documents formats, flags, timing and observable behaviour.
