# Experiment log

Lab notebook for reverse engineering AirDrop on iOS 27.2 (iPhone 17 `iPhone18,3`, "TestPhone").
Results feed `docs/PROTOCOL.md` (spec, every item tagged **measured / static / hypothesis**).

How to read it: entries are in the order they were written, and the table below is the index. A *session* is one
folder `probe/sessions/<time>-NAME/` written by `scripts/airdrop-trace.sh` (logs, packet captures, request dumps).
Those folders are not published: captures carry addresses, device names and identity records. Network names,
addresses, device names and contact-hash prefixes in this log are placeholders (`02:00:00:00:00:xx`, `aaaa`…).
Anything you can repeat is given as a command; `scripts/analyze-trace.py` turns your own session into the same numbers.

Rules for every entry:
- **ID**, question, hypothesis, and the **prediction written before the run**.
- Exact setup: commands, plus the iPhone state (network, AirDrop mode, screen, what is shared).
- **One variable** changed against a named **control**.
- What result confirms and what refutes.
- Raw result: session folder under `probe/sessions/`, frames, symbols.
- Conclusion, and the doc sections it changed.

**Where things stand (2026-11-22), newest conclusions first; search the heading to find the entry:**

| Question | Answer | Entry |
|---|---|---|
| Why is the Realtek rtw89 laptop slow (send 0.2, receive 0.56 MB/s)? | The chip holds frames for a peer without a station entry (the AP's own mac_id, 17 every ~140 ms). Station entry (OWL `-g`, default on rtw89): **send 5.5, receive 20.5 MB/s** | "E-RTW89 Phase 0" (2026-11-22) |
| Can rtw89 aggregate what we send? | **Yes: 5.5 → 40.5 MB/s** (receive 25.2) with our rtw89 build (`awdl_ba`) | "E-RTW89 F1" (2026-11-22) |
| Which open spec rows do the recorded sessions already answer? | Q-A1, Q-D1, Q-T3; three archive layouts; a second request form | "C-1" (2026-11-16) |
| Can the firmware aggregate what we send? | **Yes: 5.4 → 30.6 MB/s.** Station entry on the GO, Block Ack asked by OWL during the transfer, driver `awdl_ba`, HT/VHT elements in the GO beacon | "E-TXAGG ..." (six entries, 2026-11-15/16) |
| Can mac80211 negotiate that session? Force the width per frame? | No (never answered; stall). No (ignored) | "E-TXAGG live result", "E-TXAGG re-run with the asking fix" |
| Receive at 80 MHz? | Yes: 14.8 → 21.0 MB/s | "E-VHT80" |
| How short can a session start be? | 5.7 → 1.5 s | "Bring-up latency" |
| Fewer TCP ACKs on receive (GRO)? A-MSDU on send? | Refuted by analysis | "E-GRO", "A-MSDU on send" |
| Why were we silent 70 % of a transfer? | Our own Wi-Fi scans; scan guard | "P2.7" |
| What bounded send / receive before? | Our PHY rate / no aggregation (ADDBA unanswered) | "P2.2", "P2.3", "P2.4" |
| Background listener? | Bluetooth advert triggers one borrow session | "L1" |
| Second channel beside connected Wi-Fi? | Refuted on this card | "E0.3b" |
| Which channel? | The iPhone's dominant one, 149 | "E0.11 run 8" |

Older conclusions in entries were sometimes corrected by later ones (duty-cycle ceiling, "send is at the card's
ceiling"); the table and `docs/FINDINGS.md` are current.

Static entries (S-…) use the same format: the "setup" is the exact `ipsw` command, the "raw result" is the
symbol, string or disassembly location (names and strings only; no Apple code is copied).
Every static finding gets a paired live test where one is possible, and vice versa.

Status words: **planned**, **ready** (steps handed to the user), **done**, **blocked (reason)**.

Common iPhone setups (batch experiments that share one):
- **P-SEND**: iPhone on home Wi-Fi (ch 36), unlocked, sends to us from the share sheet. AirDrop on the
  iPhone can be anything; we are an Everyone receiver.
- **P-RECV**: iPhone on home Wi-Fi, Settings → General → AirDrop → **Everyone for 10 Minutes**, screen on,
  unlocked, on the home screen. We send to it.
- **P-OFFNET**: as P-RECV/P-SEND but the iPhone's Wi-Fi is **off** (or joined to another network), so its
  infra channel is not 36.

Per boot, before any radio experiment:
```sh
cd $PWD
sudo iw reg set <country code>
scripts/mt7921-ratefix.sh load
airdrop doctor          # must say "<country code>: channel 36 may transmit"
```

---

## Retro log: sessions before this notebook (2026-11-02/03)

These were run before the lab format. They are listed so every
measured claim has an experiment ID. Setup for all: shared mode, OWL on ch 36, `scripts/airdrop-trace.sh`.

| ID | Session | Question | Variable | Result | Conclusion |
|---|---|---|---|---|---|
| R-01 | `probe/btmon.txt` (11-02 19:59) | What does the iPhone's AirDrop BLE advert look like? | – (first look) | 85 adverts, one address, `ADV_NONCONN_IND`, `40 d691ba 00000000 03 …` | advert is non-connectable, v3 |
| R-02 | `probe/mon-*-ch{6,36}.pcap` (20:01) | What does a lone iPhone send on AWDL? | no OWL peer | sync/election/seq/election-v2 only; PSF on ch 6 | TLV set grows with a peer |
| R-03 | `probe/trace/` (21:28) | First receive | ratefix driver + regulatory domain set | `SAVED`, TLS 1.3 X25519MLKEM768 | baseline success |
| R-04 | `20261102-234351-link` | Safari link share keys | share type = link | `TransferType {links:{}}`, `Items[url]`, `DeviceSupportFlags 111611`, `SenderRecordData` | owly OQ 12 answered |
| R-05 | `20261102-234914-link2` | Does `SenderIdentityAuthTag` change per transfer? | same link, 2nd send | `4644c9` → `c7f390`; record identical | per transfer |
| R-06 | `20261103-000125-link3` | Why do attempts fail? | – | 13.7 s silence on ch 36 right after /Discover | iPhone leaves ch 36 |
| R-07 | `…-000923-link3b`, `…-001137-contact`, `…-001303/001355-photos3`, `…-001450/001513-video` | Share-type keys, archive layout | share type | archive `.`,`./f`,`TRAILER!!!` | no NSIRD_ on iOS 27.2 |
| R-08 | `20261103-001657-tlsprobe` | Does the iPhone present a client cert? | `AIRDROP_TLS_PROBE=1` (CertificateRequest) | Apple ID cert on /Ask conn, CN altDsID | Contacts Only binding |
| R-09 | `…-000923` … `…-001657` (BLE) | Is BLE tag = auth tag? | 5 transfers on one BLE address | tag `d3e32d` constant, auth tag changed 4× | not the same |
| R-10 | `20261103-011710-go-receive` | `--go 149` receive | GO on 149 | iw usage error, GO never up | fixed iw syntax |
| R-11 | `20261103-011827-go-receive` | `--go 149` receive | GO on 149, OWL verbatim sequence | GO up, Wi-Fi OK, iPhone synced, **no TCP** | verbatim sequence lies → OWL `-I` |
| R-12 | all `awdl0.pcap` (re-analysis 11-03 12:35) | What mDNS does a *sending* iPhone emit on awdl0? | – (mining) | see PROTOCOL.md §5.1 | browses 3 services, announces 2 of its own, known-answer lists, multicast flushed in bursts |

---

## E0 — second channel (unlock for every "iPhone as receiver" experiment)

**Status: ready.** Setup P-SEND for E0.1–E0.3, P-RECV for E0.4–E0.5 (same Wi-Fi, just flip AirDrop mode).

**Install first** (both installed copies are stale; checked 12:19, md5 `ed8c91eb…` / `1cdc16c6…`):
```sh
pkexec install -Dm755 -o root -g root $PWD/vendor/owl/build/daemon/owl /usr/local/lib/airdrop/owl
pkexec install -Dm755 $PWD/helper/airdrop-helper /usr/lib/airdrop/airdrop-helper
```

### E0.1 Control: shared-mode receive on ch 36 still works with the new OWL/helper
- Question: did the install break the known-good path?
- Control for: E0.3. Variable: none vs R-03.
- Prediction: `SAVED …` within 2 tries.
- Steps: `NAME=e0-1-r36 DUR=90 scripts/airdrop-trace.sh`, then on the iPhone share one photo to the laptop.
- Refuted if: the iPhone never lists the laptop, or no `SAVED`.

### E0.2 Second channel radio only — **run 11-03 13:39: refused by preflight (as designed)**
- Raw result: `airdrop doctor --go-test 149` → `FAILED: your Wi-Fi is on 2.4 GHz …`; Wi-Fi stayed connected (after: 5/5
  pings). Station was on the campus network (CampusNet): the ratefix reload re-associated it to a
  **ch 6** BSS (13:38:36, when `airdrop doctor` reported "connected on channel 6"), then NetworkManager roamed back
  to the stronger **ch 11** BSS `02:00:00:00:00:01` 20 s later (13:38:56). Installed helper/OWL still stale.
- Conclusion: the 2.4 GHz guard works. On this network E0 needs the station locked to a 5 GHz BSS.

### E0.2 (original design)
- Question: does a GO on 5745 MHz keep Wi-Fi connected? (Already OK on 11-03 at 01:18; repeated as the
  control after reinstall.)
- Prediction: `result: OK`, `during` median 20–60 ms.
- Steps: `airdrop doctor --go-test 149`

### E0.3 Receive over the second channel with OWL `-I`
- Question: if we advertise only the slots we really serve (the master's 149 slots), does the iPhone open TCP?
- Variable: OWL `-I` (intersect) vs R-11 (verbatim copy). Everything else equal (GO 149, P-SEND).
- Prediction: OWL log shows `following sync master …: k/16 slots on our channel` with k ≥ 1; the iPhone
  sends a SYN to :8771 and `/Discover`; `SAVED`.
- Steps: `NAME=e0-3-go-recv DUR=120 scripts/airdrop-trace.sh --go 149`, share one photo, then
  `grep -E "slots on our channel|NO OVERLAP" /run/airdrop-owl.log | sort | uniq -c | head`
- Refuted if: `NO OVERLAP`, or k ≥ 1 but still no SYN (then the blocker is elsewhere: mDNS, BLE).

### E0.4 iPhone as receiver: find — **run 11-03 13:42: no devices found**
- Setup: station locked to campus 5 GHz BSS `02:00:00:00:00:02` (ch 36); new helper/OWL in use:
  log shows `owl … -c 149 -N -P 10000 -I` (new OWL with `-I`). GO up on 5745, Wi-Fi held, watchdog quiet.
  iPhone: Everyone for 10 Minutes, screen on. Session `probe/sessions/20261103-134219-e0-find`.
- Raw result: `No devices found.` OWL log: 2 lines, **no peer ever added**. `air.pcap` 26 306 frames, **0 AWDL**
  (BSSID `00:25:00:ff:94:73`) from anyone. Every beacon heard is a ch 36 BSS (four campus BSSes; a neighbour network
  `02:00:00:00:00:03` has HT-op primary ch `0x24` = 36). No ch 149 transmitter heard at all. Our BLE advert was
  on air (HCI: legacy `ADV_NONCONN_IND`, 100 ms, all 3 channels, **non-resolvable** random address, payload
  `05 12 40 975264 00000000 03 00…00`) but **without a Flags AD** (iPhone sends `02 01 1a` first).
  `iw survey dump` is not usable as evidence: mt76 attributes all time to the current channel (2462 shows 0 ms
  although the station sat on it for minutes).
- Conclusion: cannot separate (a) the iPhone never woke (advert format: no Flags AD / NRPA / zero hashes) from
  (b) the radio never really receives/transmits on 149 under the GO (all RX was ch 36). → E0.3b.

### E0.3b Decisive control: iPhone as **sender**, Wi-Fi disconnected, over the second channel — **run 1 (13:52) invalid; re-run ready**
- Run 1, `probe/sessions/20261103-135211-e0-3b`: 0 AWDL frames, OWL never added a peer, awdl0 = only our own
  mDNS. **Control not met:** the LE scan ran (2170 advert reports) but there were **0 AirDrop type-0x05 adverts
  from anyone**, so the iPhone was not browsing AirDrop during the capture (every earlier share-sheet session
  showed its advert continuously). Inconclusive; re-run with the share sheet confirmed open.
  Side note: `BrokenPipeError` on stdout at exit (cosmetic, log saved).
- **Run 2 (13:55) — valid, REFUTES the second channel on this card.** `probe/sessions/20261103-135550-e0-3b2`.
  Control met: the iPhone advertised AirDrop for 114 s (480 type-0x05 adverts, 20.9–134.5 s, address
  `02:00:00:00:00:04`, tag `bc05c3`, its hash set in 2 shuffled orders), so its AWDL was up; its Wi-Fi was
  disconnected, so its sequence had no ch 36. Raw result: **0 AWDL frames** in `air.pcap` over 120 s; OWL never
  added a peer; no iPhone packet on awdl0; "laptop" never appeared on the iPhone (user: "no").
  Conclusion: under the P2P-GO the MT7921 monitor never receives on 149 (all RX in E0.4/E0.3b stayed on the
  station's ch 36). Consistent with R-11 (home, iPhone infra 36: it synced on 36, never connected). **E0 via the
  GO is refuted on this hardware/firmware.** E0.5 (send via `--go`) is moot. Next: E0.7 (station on a social
  channel, no GO).

### E0.7 Shared mode on a social channel (ch 6), no second channel — **ready (home or campus)**
- Question: with the station genuinely on ch 6 (the iPhone's AWDL master channel; PSFs there, slot 8 of every
  sequence is ch 6), can we (a) hear a sending iPhone, (b) find a receiving one (Q-D1), (c) send to it?
- Setup: the station must sit on a **ch 6** BSS (`iw dev wlan0 link` → `freq: 2437`); plain shared mode, no `--go`.
  `airdrop doctor` must print `[ok ] sending to an iPhone`. Three ways (ask before each profile change):
  - **Home, borrow mode (no router change; user OK 2026-11-04 for send/receive sessions):** add `--mode borrow`
    to every command below, e.g. `NAME=e07b DUR=60 scripts/airdrop-trace.sh find --mode borrow -t 45`. The helper
    disconnects Wi-Fi through NetworkManager (wlan0 stays up and managed), runs AWDL on ch 6, and reconnects the
    same connection at teardown (also `airdrop recover`). Needs the reinstalled helper. Check after each run:
    `nmcli -t -f GENERAL.STATE device show wlan0` → `100 (connected)`.
  - **Home:** the router's 2.4 GHz `HomeNet` (`02:00:00:00:00:05`) is on **ch 1** (scan 2026-11-04), and the
    same SSID also has a 5 GHz BSS (ch 36). Step 1 (you, router admin page): set the **2.4 GHz channel to 6**
    (not Auto). Step 2 (laptop), pin the profile to 2.4 GHz and switch to it (Wi-Fi reconnects for a few seconds):
    `nmcli con modify HomeNet 802-11-wireless.band bg && nmcli con up HomeNet`.
    Back to normal afterwards: `nmcli con up HomeNet-5GHz` (the 2.4 GHz pin can stay).
  - **campus:** `nmcli con modify CampusNet 802-11-wireless.bssid 02:00:00:00:00:06 && nmcli con up CampusNet`;
    undo: `nmcli con modify CampusNet 802-11-wireless.bssid "" && nmcli con up CampusNet`.
  - The iPhone itself stays on whatever network it likes (at home: the 5 GHz one, so its infra is ch 36).
- Control: E0.1/R-03 (shared receive on ch 36 at home works). One variable: our channel = 6.
- **(a)** iPhone on campus Wi-Fi (any BSS), shares one photo to "laptop":
  `NAME=e07a DUR=120 scripts/airdrop-trace.sh` → prediction `SAVED …` (first 2.4 GHz receive); OWL log
  `following …`. Refuted: no peer in `/run/airdrop-owl.log` despite iPhone adverts in `ble.btsnoop`.
- **(b)** iPhone: Settings → General → AirDrop → Everyone for 10 Minutes, screen on, unlocked:
  `NAME=e07b DUR=60 scripts/airdrop-trace.sh find -t 45` → prediction `TestPhone … available`; dump holds
  `NNN-mdns.json` (TXT, Q-D1) and `NNN-Discover-reply.json` + `…-ReceiverMediaCapabilities.bin`. If (a) works and
  (b) shows no iPhone AWDL frames at all, the BLE wake is the blocker (Q-B4 → E1.2).
- **(c)** only if (b) found it: `NAME=e07c DUR=120 scripts/airdrop-trace.sh send ~/Downloads/AirDrop/IMG_1571.jpg`
  (type the iPhone's number + Enter; accept on the iPhone) → prediction `Sent`. Refuted: `/Ask` unanswered → diff
  our `/Ask` against the iPhone's own (R-07).
- **(c) sender form (2026-11-04):** `/Discover` = `{DeviceSupportFlags: 139}` (was `{}`), `/Ask` = the iPhone's
  measured key set minus `FileIcon`, `SenderRecordData`, `SenderIdentityAuthTag`; `/Ask` waits 180 s for Accept.
- **(c) also answers Q-T3:** our `/Ask` carries no `SenderRecordData` and no `SenderIdentityAuthTag`; `Sent` =
  an iPhone receiver does not require them in Everyone mode; a decline/error right after `/Ask` = it does.
- **(d) Contacts Only (Q-T2)**, only if (b) found it: iPhone → Settings → General → AirDrop → **Contacts Only**,
  screen on: `NAME=e07d DUR=60 scripts/airdrop-trace.sh find -t 45`. Read: `ls probe/sessions/*-e07d/dump/*/` and
  `grep -h -E 'Receiver|status' probe/sessions/*-e07d/dump/*/*Discover-reply.json`. Prediction: iPhone either not
  listed, or listed "contacts only (not reachable)" with a `/Discover` reply lacking `ReceiverComputerName`.
- Free by-products of the same captures (no extra run): iPhone's TLV 0x21 bytes 4–5 on a 2.4 GHz infra (E2.2;
  `python3 scripts/awdl-decode.py probe/sessions/*-e07a/air.pcap --tlv 0x21`), a receiving iPhone's channel
  sequence (E2.4; `--tlv 0x12` on `*-e07b`), election outcome with our default metric 60 (E2.6 control; `--tlv 0x18`).

### E0.9 Borrow mode safety check (no iPhone) — **run 1 done; run 2 ready**
- Question: does `--mode borrow` move AWDL to ch 6 and always give Wi-Fi back?
- Command: `airdrop -v find --mode borrow -t 10; sleep 5; nmcli -t -f GENERAL.STATE device show wlan0; ping -c 3 1.1.1.1`
- **Run 1 (2026-11-04):** NM disconnect OK; `iw dev awdlmon set channel 6` → **EBUSY (-16)** with wlan0 up
  (cfg80211 tunes a monitor only when all running interfaces are monitors); helper aborted as designed
  ("radio did not move to channel 6"); restore deleted the vifs, restored PM, reconnected the same UUID;
  `100 (connected)`, ping 0 % loss [M]. Fail-safe path confirmed.
- Fix: borrow now also sets wlan0 link **down** for the session (still NM-managed; recorded before; restore
  brings it up, then reconnects with retries; watchdog stops if wlan0 comes up mid-session).
- Run 2: same command after reinstalling the helper. Prediction: `AWDL up on awdl0 (borrow mode, channel 6)`,
  then `100 (connected)` and a clean ping.
- **Run 2 (2026-11-04):** with wlan0 link down, `iw dev awdlmon set channel 6` **succeeded** (no EBUSY) and the
  monitor received frames [M]; the helper still aborted because `iw dev awdlmon info` prints **no channel line**
  for a monitor on this driver (our check read `None`). Restore again complete: same UUID, `100 (connected)`,
  ping 0 % loss [M]. Fix: confirm the channel from the radiotap frequency of frames the monitor hears
  (`tcpdump -e`; parser checked on a stored capture: 20/20 frames → 5180 MHz).
- Run 3: same command after reinstalling the helper. Prediction: `radio confirmed on 2437 MHz` (in -v),
  `AWDL up … (borrow mode, channel 6)`, Wi-Fi back.
- **Run 3 (2026-11-04):** the radiotap check worked and caught the real problem: after `set channel 6`
  (rc 0) the monitor still heard **5180 MHz** — the radio stayed on ch 36 with wlan0 down [M]. Restore again
  complete, ping OK. Source reading (mt76 7.2.5): mt7921 with CNM firmware uses real chanctx ops and
  `WANT_MONITOR_VIF`; our active monitor (`flags active`, a real driver vif) is created after the tune and
  presumably gets its own context on the old channel, which the single-channel radio follows [H].
- Fix: tune every vif after both exist, re-check; if still wrong, drop the active vif and retry with the plain
  monitor (warning: no ACKs). Run 4: same command after reinstalling.
- Policy since 2026-11-04 (user: "the best option for AirDrop"): `find`/`send` in the default `auto` mode use
  borrow by themselves whenever Wi-Fi is not on ch 6; `receive` stays shared. In borrow mode OWL runs with `-I`
  (advertise only the master's ch 6 slots; R-11: advertising unserved channels = sync but no TCP). So E0.7b/c
  need no `--mode` flag at home.

- **Run 4 (2026-11-04):** retune of both vifs, then plain monitor only: still **5180 MHz**; restore OK [M].
  Source reading (mt76 7.2.5, CNM firmware): `assign_vif_chanctx` only records the context; the monitor is tuned
  (`mt7921_mcu_config_sniffer`) only in `change_chanctx`, i.e. when an existing context *changes*; and mac80211
  ignores a `set channel` equal to the stored monitor channel. So "new monitor context on ch 6" may never reach
  the firmware [H]. Remain-on-channel (`mt7921_set_roc`) is the one CNM path that tunes on request.

### E0.10 What actually moves this radio to ch 6? (manual probe, no helper) — **ready**
- Variables, one per line, each followed by a 4 s capture of the frequencies the monitor hears (per-frame,
  from the RX descriptor): A start, B `set channel 6`, C `set channel 6 HT40+` (width change), D `set channel 1`
  (home router's 2.4 GHz: beacons expected), E wlan0 up + `iw dev wlan0 offchannel 2437 4000` (ROC).
- Prediction [H]: A–D stay on 5180; E shows 2437. If B or C shows 2437, the helper just needs that order.
- Safety: one zsh subshell; a trap (on exit, Ctrl-C or error) deletes the monitor, restores PM, brings wlan0
  up and reconnects the saved UUID, then prints WIFI-BACK.
  ```sh
  ( P=$(basename $(readlink -f /sys/class/net/wlan0/phy80211)); D=/sys/kernel/debug/ieee80211/$P/mt76
    C=$(nmcli -t -f UUID,DEVICE con show --active | awk -F: '$2=="wlan0"{print $1}'); sudo -v || exit 1
    trap 'exit 1' INT TERM
    trap 'sudo iw dev m0 del 2>/dev/null; echo 1 | sudo tee $D/runtime-pm $D/deep-sleep >/dev/null; sudo ip link set wlan0 up; nmcli con up uuid "$C" >/dev/null && echo WIFI-BACK' EXIT
    f() { echo "== $1"; sudo timeout 4 tcpdump -i m0 -c 30 -e -nn -q 2>/dev/null | grep -o "[0-9]\{4\} MHz" | sort | uniq -c; }
    nmcli dev disconnect wlan0 >/dev/null; sudo ip link set wlan0 down
    sudo iw phy $P interface add m0 type monitor && sudo ip link set m0 up
    echo 0 | sudo tee $D/runtime-pm $D/deep-sleep >/dev/null
    f A-start
    sudo iw dev m0 set channel 6;       f B-ch6
    sudo iw dev m0 set channel 6 HT40+; f C-ch6-HT40
    sudo iw dev m0 set channel 1;       f D-ch1
    sudo ip link set wlan0 up; sleep 1
    (sudo iw dev wlan0 offchannel 2437 4000 &) ; sleep 0.3; f E-roc-ch6 )
  ```
  If WIFI-BACK is missing: `sudo ip link set wlan0 up; nmcli device connect wlan0`.
- **Run 1 (2026-11-04):** A 30×2412 · B 30×2412 · C 30×2412 · D 30×2412 · E 5×2447, 4×2452, 1×2457, 20×5180;
  WIFI-BACK [M]. Conclusion: a fresh monitor (wlan0 down) sits on ch 1; **`set channel` on a running monitor never
  reaches the firmware** (B, C). E is contaminated: NetworkManager scans when wlan0 comes up (2447–2457, then
  5180); no 2437 frame. New hypothesis: the radio takes the monitor context's channel only when the monitor
  comes up → set the channel while the monitor is down.
- **Run 2 (ready):** F/G = monitor down → `set channel` → up (6, then 11 as a second point); B2 = `set channel 6`
  on the running monitor again (control); H = wlan0 up, 12 s for NM's scan to end, capture; E2 = ROC 2437 with
  `iw` output shown. Same trap/restore as run 1.
- **Run 2 (2026-11-04):** A 30×2412 · F `set channel` while down → **EBUSY**, 30×2412 · G EBUSY, 30×2412 ·
  B2 30×2412 · H 1×5825 (scan) · **E2 ROC 2437 → 30×2437** · WIFI-BACK [M].
  Conclusions: (1) no userspace order makes the stock driver apply a monitor channel; (2) remain-on-channel on
  wlan0 (up, disconnected) does put the radio on ch 6 and the monitor hears it.
- **Fix chosen (user asked for a driver patch):** `vendor/mt76-driver/patch/mt7921-monitor-channel.diff`: in
  mt7921-common, `assign_vif_chanctx` for a monitor vif also runs the sniffer enable + `mt7921_mcu_config_sniffer`
  (what `change_chanctx` already does). Built out of tree against 7.2.5 (clang, vermagic matches; no
  MODVERSIONS): `vendor/mt76-driver/mt7921-common-monchan.ko` (srcversion `1E74B5FD…`). Loaded with
  `scripts/mt7921-ratefix.sh load-monitor` (both modules, stock restored on failure; a reboot reverts).
  Fallback if the patch fails: ROC-based borrow (E2 shows it tunes; TX/ACK under ROC untested).
- Next: E0.9 run 5 = the borrow check with the patched driver. Prediction: `radio confirmed on 2437 MHz (awdlact)`.
- **E0.9 run 5 (2026-11-04):** `load-monitor` OK (both patched modules loaded, Wi-Fi reconnected, radio now
  `phy1`). Borrow check: still **5180 MHz** after both vifs + retries; restore OK, ping 4.5 ms [M]. Not decisive:
  the helper measures only after the active vif exists, and repeated `set channel 6` is a mac80211 no-op.
- **E0.10 run 3 (ready, patched driver):** A start · B m0→6 · C m0→11 · D m0→6 · E add active monitor m1 ·
  F m1→11 · G m0→1 then →6 (forced change). Isolates "patch works on the plain monitor" vs "active vif pulls the
  radio back".
- **E0.10 run 3 (2026-11-04):** A–G all 30×2412, WIFI-BACK [M]. **The monitor-channel patch has no effect**: the
  CNM firmware does not move the radio on `mt7921_mcu_config_sniffer` (or the path is not reached). Patch kept in
  `vendor/mt76-driver/patch/` as a refuted attempt; not needed.
- **Borrow mode v2 (remain-on-channel):** wlan0 is only NM-disconnected (stays **up**, no link-down any more); a
  helper thread starts `iw dev wlan0 offchannel 2437 5000` every 4 s (mt76 max ROC 5000 ms; time-driven, so it
  does not matter whether `iw` blocks); the radiotap check must see 2437 before OWL starts; teardown stops the
  thread, the last ROC expires ≤ 5 s, NM reconnects the same UUID. Next: E0.9 run 6.
- **E0.9 run 6 (2026-11-04):** borrow v2 heard **5180** (ROC started 1 s after the NM disconnect); restore OK,
  ping fine [M]. Difference from the E0.10 run 2 success: there wlan0 was idle 12 s first (NM's scan done).
  Change: 3 s settle after the disconnect, verify up to 4× over ~7 s, log the first ROC exit codes and the heard
  frequency histogram. Next: E0.9 run 7.
- **E0.9 run 7 (2026-11-04): PASS.** ROC `rc=0` every 4 s; monitor heard **2437 MHz ×12**; `radio confirmed on
  2437 MHz via remain-on-channel (awdlact)`; OWL `-c 6 -N -P 10000 -I`; `AWDL up on awdl0 (borrow mode, channel 6
  + BLE)`; teardown reconnected the same UUID, `100 (connected)`, ping 0 % loss [M]. "No devices found" (no
  receiving iPhone in this run). Stale `--go 149` hint replaced. **Borrow mode is ready for E0.7b.**

### E0.8 Off-channel hops instead of a second channel (user's idea, 2026-11-04) — **planned (feasibility probe)**
- Question: can the MT7921 visit ch 6 for short windows while the station stays associated on ch 36, using
  mac80211 remain-on-channel (`iw dev wlan0 offchannel`; the phy lists `remain_on_channel`), and do our monitor
  vifs hear the iPhone's ch 6 AWDL frames during those windows?
- Why it might not be enough even if it works: AWDL slots are 16 TU and every HTTP exchange is many TCP round
  trips; userspace-timed hops (ROC setup takes milliseconds) are coarse compared with AWDL's firmware-timed
  sequence. If the probe hears frames, step 2 is OWL asking for ROC in the iPhone's ch 6 slots.
- Risk: the station leaves ch 36 for ≤ 200 ms per hop (mac80211 tells the AP it is dozing); association stays up.
  No exclusive mode, no GO.
- Setup: home, iPhone sends (share sheet open on a photo, AirDrop on; its AWDL then sends PSFs on ch 6).
  Terminal 1: `NAME=e08 DUR=90 scripts/airdrop-trace.sh`. Terminal 2, once AWDL is up:
  `for i in $(seq 20); do sudo iw dev wlan0 offchannel 2437 200; sleep 1; done; ping -c 3 1.1.1.1`.
- Read: `tcpdump -e -nn -r probe/sessions/*-e08/air.pcap 2>/dev/null | grep 00:25:00:ff:94:73 | grep -c "2437 MHz"`
  (AWDL frames heard on ch 6) and the ping (Wi-Fi alive). Prediction [H]: some PSFs heard on 2437 (confirms RX
  follows ROC); refuted if 0 (then the monitor vifs don't follow ROC, same wall as E0.3b).
- Control: E0.3b (0 AWDL frames on the GO's channel); the station-on-ch 6 setup of E0.7 doesn't need any of this.

### E0.7b run 1 (2026-11-04, home, borrow mode on ch 6) — **partial: iPhone seen, TCP silent**
- Session `probe/sessions/20261104-174509-e07b`. iPhone: Everyone for 10 Minutes, on home Wi-Fi (ch 36).
- Raw [M]: OWL `add peer 02:00:00:00:00:07`, `following sync master …: 1/16` then `2/16 slots on our channel`.
  iPhone sequence `36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36` → after 32 s `6,0,149,0,0,0,0,0,6,0,149,0,0,0,0,0`
  (ch 36 dropped, ch 6 added in slot 0). iPhone TLVs: 0x04 0x05 0x06 0x07 0x0c 0x10 0x11 0x12 0x15 0x18 0x21.
  **iPhone mDNS as receiver:** probe `ANY? 0b48a1283375._airdrop._tcp.local.`, then announcement **TXT
  `flags=111611`**, PTR `_airdrop._tcp` → `0b48a1283375._airdrop._tcp.local.`, SRV
  `b2074e1c-9830-48bc-81c1-e9a93962021a.local.:8770` (hostname = the UUID OWL shows as its arpa name; it rotated
  to `4ea40820-…` at +32 s). Our 4 SYNs to :8770 and 8 pings: **no answer**. air.pcap: **0 frames with TA or RA
  = our MAC** (working ch 36 sessions: 37/128 and 196/443) → our injected frames were never transmitted.
- Conclusions: Q-D1 TXT answered; TXT `flags` = the same value as `DeviceSupportFlags` (111611 = 0x1B3FB). TX
  blocked: hypothesis = NetworkManager randomizes wlan0's MAC while disconnected, so injected frames (sent via the
  vif whose MAC = their TA) no longer match wlan0 and do not go out under wlan0's ROC.
- Fix: runtime NM rule `wifi.scan-rand-mac-address=no` for wlan0 during borrow (`/run/…/90-airdrop-borrow.conf`,
  removed on teardown), vifs copy wlan0's MAC after the disconnect, MAC-drift warning. Run 2: same command.

### E0.7b run 2 (2026-11-04) — **same result with a stable MAC**
- Session `…-175015-e07b2`. NM rule worked: wlan0 MAC stayed `02:00:00:00:00:08`, vifs copied it. iPhone: peer at
  once with `6,0,149,0,0,0,0,0,6,0,149,0,0,0,0,0`; mDNS probe + announce of a **new instance** `691123324211`
  (run 1: `0b48a1283375`) → the instance name is per session; TXT `flags=111611` again. 30 SYNs to :8770, no answer;
  no unicast from the iPhone [M].
- Correction to run 1: "0 frames with TA = us" is **not** proof of no TX. In working sessions those frames were
  wlan0's traffic to the AP; our AWDL frames are visible in some captures (contact: 42) and not others (photos3:
  0). Open: (a) no unicast TX under ROC, or (b) a receiving iPhone ignores our TCP.

### E0.11 Does our unicast go out under remain-on-channel? (iPhone sends, borrow mode) — **ready**
- Variable vs every working receive (shared, ch 36): borrow mode on ch 6 (ROC). The iPhone *initiates* TCP, so
  a completed handshake needs our SYN-ACK on air.
- Command (iPhone on home Wi-Fi, share one photo to "laptop"):
  `NAME=e011 DUR=120 scripts/airdrop-trace.sh --mode borrow`
- Read: `receive.log` (`SAVED …`?) and `tcpdump -nn -r probe/sessions/*-e011/awdl0.pcap 'tcp[tcpflags] & tcp-syn != 0'`.
- Prediction: if SAVED or SYN→SYN-ACK→ACK completes → (a) refuted, the E0.7b problem is the iPhone's acceptance (b).
  If iPhone SYNs arrive but our SYN-ACKs are never answered → (a) confirmed: TX under ROC does not air.

- **E0.11 run 1 (2026-11-04):** session `…-175321-e011`. iPhone peer at once; on opening the share sheet its
  sequence became sender-like (`149×5,0,0,0,6,149…` → 1/16 on ch 6), later 36-heavy. **No packet at all from the
  iPhone to us** (no TCP, no mDNS query; only its own announcements) — unlike every shared-mode session on ch 36.
  Conclusion: (a) confirmed in practice — under ROC with wlan0 disconnected our frames do not reach the iPhone.
  Hypothesis: mt7921 firmware sends only for an active BSS; wlan0's is inactive while disconnected.
- Fix: borrow `--borrow-radio go` (new default): station disconnected, hidden P2P-GO on ch 6 as the only channel
  context (single channel → RX on 6, unlike E0.3b), vifs carry the GO MAC so mac80211 injects via the GO.
  `--borrow-radio roc` keeps the ROC variant. Run 2 = same command after reinstalling.

- **E0.11 run 2 (2026-11-04, `--borrow-radio go`):** GO `awdlgo` up on 2437, `radio confirmed on 2437 MHz via
  go`; OWL peer `3a:b2:…` (iPhone seq `36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36`, 1/16 on ch 6). The iPhone
  **browsed** (`PTR? _airdrop._tcp.local.` QM at +3 s, +16 s, …) while we sent 83 mDNS packets, but never sent
  known answers or SRV/TXT queries for our instance, no SYN; "laptop" not in the share sheet; restore OK [M].
  So the iPhone received none of our packets via the GO either. Open: injected frames not on air at all, vs.
  aired but outside the iPhone's single ch 6 slot.

### S-23 Why the iPhone gets nothing from us in borrow mode (static, our own code + captures) — **answered (OWL bug)**
- Our TX-status echoes (`[bit 15]` radiotap TX flags, unicast AWDL data, DA = iPhone) are the visible proof of TX:
  contact (shared, works) 42; e011b (GO) **1**, while awdl0 handed OWL 27 unicast packets; all ROC runs 0.
- OWL source: multicast leaves only in multicast EAWs (slots 0, 10) and, with `-I`, only if our slot there is
  on the radio's channel. A *sending* iPhone has ch 6 only in slot 8 (`36,36,149,0,0,0,0,36,6,…`), so with `-I`
  our slots 0/10 are empty → **multicast can never leave**. `poll_host_device` stops reading awdl0 while the
  multicast queue (16) is full → **every unicast frame is stuck behind it** (head-of-line block): exactly one
  unicast frame in 120 s in e011b. (A *receiving* iPhone has ch 6 in slot 0 → no block; there the ROC runs still
  sent 0 → ROC TX itself is blocked, consistent with the inactive-BSS hypothesis.)
- Fix (OWL, `packaging/owl-airdrop.patch` regenerated): (1) keep reading awdl0 when the multicast queue is full,
  drop and free the oldest multicast frame (`tx_data_multicast_dropped`); (2) with `-I`, if neither slot 0 nor 10
  is ours, multicast may use any slot we are on. Offline harness (scratchpad `mctest`, measured sequences):
  sender → slot 8 (was: never), receiver → slot 0 (unchanged), ch 6 only at 10 → slot 10.
- Paired live checks: E0.7b run 3 (GO, iPhone receives — slot 0 is ch 6, so this tests GO TX even without the
  fix) and E0.11 run 3 (GO + fixed OWL, iPhone sends).

### E0.7b run 3 / E0.11 run 3 — how to judge (after reinstalling helper + OWL)
- One command per session (our AWDL MAC = the GO's, printed as `borrow: GO awdlgo <mac>`):
  `S=$(ls -d probe/sessions/*-e07b3 | tail -1); python3 -I scripts/analyze-trace.py $S $(grep -o 'GO awdlgo [0-9a-f:]*' $S/*.log | awk '{print $3}') | head -8`
- Baselines: working shared sessions (contact / link2) "our unicast data frames to peer" 42 / 59, "ACKs addressed
  to us" 42 / 59; e011b (GO, old OWL) 1 / 0. Our own action frames are never echoed (0 even when working), so
  that line is no evidence either way.
- Success = ACKs addressed to us > 0 and, on awdl0, SYN → SYN-ACK; then `find` lists `TestPhone … available`.

### E0.11 run 3 (2026-11-04 21:53, borrow GO, OWL deadlock fix) — **MILESTONE: first iPhone→Linux transfer over ch 6**
- Session `probe/sessions/20261104-215345-e011c`. Network NetB (ch 40); borrow mode → GO on ch 6; OWL
  `d41f6854` (S-23 fix) reinstalled; ratefix 7.2.5 loaded.
- Raw [M]: `radio confirmed on 2437 MHz via go`; iPhone discovered us ~80 s after AWDL up, then **POST /Discover,
  POST /Ask, POST /Upload** all arrived (first ever from a receiving-then-sending iPhone over ch 6). The S-23
  deadlock is gone — our mDNS/replies reached the iPhone.
- Transfer: `IMG_1259.jpg`, 7 630 622 B, `ConvertMediaFormats False`. Phone sent **6.27 MB in 4569 segments**,
  ~220–250 KB/s for the first ~24 s, then ~12 s of a 0.7 s-gap trickle, then the **iPhone RST at 21:55:42** —
  12 s before the DUR=120 cutoff (21:55:54), so the phone gave up, not the trace. On disk:
  `.IMG_1259.jpg.airdrop-part` 6 036 652 B (spool renamed only on completion). No `SAVED`.
- Conclusion: the ch 6 path (BLE wake + GO + fixed OWL) carries a real AirDrop transfer most of the way.
  Open: it does not complete — throughput collapses late. Candidates: the iPhone periodically leaves ch 6
  (R-06; our single-channel GO can't follow) → TCP stalls → iPhone resets; or an AWDL send-window ceiling.
  Next: E0.11 run 4 — a **small** file (~1 MB) and `DUR=300`, to get one *complete* transfer; then study the
  stall with a large file. E2.8/E2.9 (tell the iPhone we are 2.4 GHz-only / infra ch 6) may widen our ch 6 dwell.

### E0.11 run 4 (2026-11-04 22:01) — **FIRST COMPLETE iPhone→Linux transfer** ✅
- Session `…-220125-e011d`. Small photo `IMG_1593.jpg` **558 445 B**: `Received 558.4 KB from TestPhone in 4.4s`,
  `SAVED`, file verified (JPEG, iPhone 17, iOS 27.2 EXIF, declared size == saved, no `.airdrop-part` left) [M].
  End to end over ch 6 in borrow mode. ~127 KB/s.

### E0.11 run 5 (2026-11-04 22:02) — big file fails, iPhone retries
- Session `…-220230-e011d`. `IMG_1590.jpg` **11 564 834 B**. iPhone ran /Ask+/Upload, reset, then **retried the
  whole transfer 3–4×** (fresh /Ask each: "reset by peer", "closed inside chunk", "0 bytes read"); phone showed an
  error; no `SAVED`. Sent **17.7 MB** total across attempts (re-sent the file) [M]. Slow throughput.
- So: **small files complete (run 4), large files time out (runs 3, 5).** Cause [H]: under the single-channel GO
  the iPhone dwells on ch 6 only part of each cycle (we advertise dual-band + no infra channel, so it keeps slots
  for 5 GHz too, S-24); a big transfer exceeds the iPhone's per-attempt deadline.
- Next: E0.11 run 6 — reinstall the helper (for the DPS knobs), `AIRDROP_HELPER_ARGS="--dps-single-band
  --dps-infra-channel"` (E2.8+E2.9: tell the iPhone we are 2.4 GHz-only, infra ch 6 → it should keep more ch 6
  slots) + the same big file + `DUR=300`. Read throughput from `receive.log` / awdl0.pcap.

### E0.11 run 6 (2026-11-04 22:06, borrow + `-S -C 6`) — DPS knobs help; ~9 MB ceiling found
- Session `…-220619-e011e`. OWL `-S -C 6` (single-band DPS, infra channel 6). Same 11.56 MB `IMG_1590.jpg`.
- Result [M]: **one clean /Ask+/Upload (no retry storm)**, phone sent **9.92 MB in 48.2 s = 201 KB/s**, only 4 gaps
  >1 s (max 2.7 s), then reset; no `SAVED`. (Phone UI showed "complete" ~15 s later — optimistic; file not on disk.)
- vs run 5 (no knobs): 6 MB then 3–4 retries. So `-S`/`-C` **widen the ch 6 dwell** (6→9.9 MB, retries gone) —
  exactly the steer S-24 predicted. Steady ~200 KB/s with small gaps now, not a trickle.
- **Interpretation:** the limit is an **iPhone per-transfer deadline of ~45–50 s**; at ~200 KB/s that caps a
  transfer near **~9 MB**. Small/medium files complete (run 4: 558 KB ✅); ~11.5 MB just overruns. Throughput is
  capped by AWDL time-sharing (iPhone on ch 6 only part of each 1 s cycle), which one radio can't fully beat.
- **Change:** `-S -C <ch>` are now the **default in borrow mode** (it is genuinely single-band on one channel).
- Cheap next tests: (a) phone next to the laptop (higher rate may finish 11.5 MB in time); (b) confirm the ceiling
  with a ~7–8 MB file (should complete). Bigger lever (unbuilt): HT40 GO + HT caps so the iPhone sends faster.

### E0.11 run 7 (2026-11-04 22:19) — **2-stream confirmed: iPhone now MCS 15** ✅ (lever 1 works)
- Session `…-221938-e011g`, 11.56 MB file. iPhone moved from MCS 7 (1 stream, run 6) to **MCS 15 ×6417, MCS 13-14**
  (2 streams, ~144 Mbit/s HT20-SGI) [M] — our 2-stream HT caps were honoured. Transferred **11.31 / 11.56 MB (98 %)**
  in 45.4 s = 243 KB/s, then reset at the ~45 s deadline. So close.
- Lesson: per-frame rate ~2.2×, but effective rate only 201→243 KB/s — **duty-cycle bound** (faster bursts, same
  ~2-3 % airtime, bigger idle gaps). The ~45 s deadline is the wall. Need ~2× more bytes/slot ⇒ **HT40** (lever 2).

### E0.11 run 8 (2026-11-04 22:30) — **11.5 MB COMPLETE on 149 in 17.6 s** ✅✅ (the real fix)
- Session `…-223050-e011h`, `--channel 149`. `Received 11.6 MB in 17.6s`, `SAVED`, file byte-exact (11 564 834) [M].
  **~660 KB/s = 2.7× ch 6**, iPhone at MCS 15 (144 Mbit/s). The file that failed 4× on ch 6 completed first try.
- Confirms: sit on the iPhone's **dominant** channel (149, its 5 GHz social channel) → far more slots caught.
  **149 is now the borrow-mode default** (helper); `--channel 6` for a 2.4 GHz-infra iPhone.

### E0.11 run 9 (2026-11-04 22:39, `--wide`) — HT40 engages (300 Mbit/s) but throughput flat → **duty cycle is the wall**
- Session `…-223904-e011i`. GO HT40+ on 149, OWL `-H`. iPhone sent at **300 Mbit/s (MCS 15 HT40, 5441 frames),
  270/243/240/…** — HT40 2-stream confirmed on-air, 2× the HT20 rate. 11.9 MB PNG **SAVED** (byte-exact), 21.0 s.
- But effective rate **567 KB/s = no better than HT20 (660)**. Airtime math: 11.9 MB at 300 Mbit/s = 0.32 s of
  21 s = **1.5 % duty cycle**. The PHY can do 37.5 MB/s; we get 0.58 because the iPhone is on our channel ~1.5 %
  of the time. **Rate is maxed; the sole remaining bottleneck is duty cycle (airtime on the shared channel).**
- Therefore VHT80 is NOT worth building (it raises per-frame rate, which we proved does not move throughput).
  `--wide` stays opt-in (completes files by fitting the ~45 s deadline a little better; more spectrum, marginal).
- The ceiling for native speed is airtime, reachable only by: (a) a **2nd Wi-Fi adapter** so one radio holds the
  iPhone's channel full-time (clean, removes the time-share), or (b) getting the iPhone to commit its AWDL
  availability to our channel (data-path negotiation OWL does not implement; near the auth boundary). No further
  single-radio software lever remains.

### E0.13 (2026-11-05 10:28) — **first Linux → iPhone send completes (borrow mode, ch 149)**
- Command: `AIRDROP_HELPER_ARGS="--realtime --wide" NAME=send1 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone ~/Downloads/AirDrop/IMG_1571.jpg`
  (iPhone: AirDrop Everyone for 10 Minutes, unlocked). Session `probe/sessions/20261105-102814-send1`. `send` accepts `--mode borrow`.
- Result [M]: GO on 5745 MHz up, OWL `-c 149 -I -S -H -R -C 149`, BLE advert, TestPhone found, `/Ask` for
  `IMG_1571.jpg` (2 509 294 B) accepted, **`Sent 2.5 MB to TestPhone in 27.7s (90.6 KB/s)`**. Wi-Fi reconnected cleanly.
- Timeline from dump mtimes: mDNS/`/Discover` 10:28:32 → `/Ask` 10:28:38.7 → accept reply 10:28:42.5 (3.8 s) →
  `/Upload` reply 10:29:10.2 (27.7 s). Upload rate ~10× below receive (E2.10b 1.43 MB/s).
- Not verified: that the photo appeared in the iPhone's Photos. Open: why send is slow (duty cycle on the iPhone's
  receive side? Linux TX rate?), larger files. Read `awdl0.pcap`/`air.pcap` with `scripts/awdl-decode.py`.

### E2.10b (2026-11-05 01:03) — **43 MB video completes; `--realtime --wide` is now the borrow default**
- Session `…-010303-e210a`, borrow on 149 with `-R -H` (real-time + HT40). Two transfers back-to-back on one
  AWDL session: 1.1 MB photo in 1.0 s, then **43.4 MB `.MOV` in 30.3 s = 1.43 MB/s** [M] — the biggest file yet,
  SAVED and byte-exact. Duty cycle ~3.8% (vs 2.2% with -R only, 1.5% HT20). The combo is the best single-radio
  result; multiple files in one session also work (peer held across transfers).
- **Made default (2026-11-05):** in borrow mode the helper now sets HT40+ GO on 5 GHz and OWL `-R -H` by default
  (with `-I -S -C 149`). `--narrow` forces HT20; `--wide`/`--realtime` are now accepted no-ops. `airdrop receive`
  (auto) therefore runs the full tuned stack with no flags. **Reinstall the helper** to pick this up.

### E2.10 (2026-11-04 23:02) — **DPS real-time bit works: +45% airtime** (approach 2 partial win)
- Result [M]: same 11.5 MB file, **17.6 s (run 8) → 14.3 s = 831 KB/s**; duty cycle **1.5% → 2.2%**; iPhone sequence
  more 149-dominant (21 slots 149 vs 7 ch 6). Setting the DPS real-time bit (0x2000) made the iPhone commit more
  airtime to our channel. **Now default in borrow mode** (helper), with `-H`/`-S`/`-C`/149.
- Ceiling remains ~2% duty (native would be ~50%+): the iPhone still does not fully dedicate without its NAN
  data-path handshake (out of bounds). Approach 2 gives a real but partial gain; a 2nd adapter remains the only
  route to native speed. Levers exhausted for a single radio in-bounds.

### E2.10 orig — DPS real-time bit (approach 2, duty-cycle lever)
- Rationale: throughput is airtime-bound (1.5 %, run 9). The iPhone bursts during its availability windows but does
  not dedicate the channel. It sets the data-path **real-time bit 0x2000** (`infraRealTimeActive`, S-20) during its
  OWN transfers. Hypothesis: if WE set it, the iPhone treats the path as real-time and commits more airtime.
  Built: OWL `-R` (DPS flags |= 0x2000), helper `--realtime`. In-bounds (a flag, channel/behaviour; not auth).
- Reinstall both, then (iPhone sends a large file on 149, the default):
  `pkexec install -Dm755 $PWD/helper/airdrop-helper /usr/lib/airdrop/airdrop-helper`
  `pkexec install -Dm755 -o root -g root $PWD/vendor/owl/build/daemon/owl /usr/local/lib/airdrop/owl`
  `AIRDROP_HELPER_ARGS="--realtime" NAME=e210a DUR=300 scripts/airdrop-trace.sh --mode borrow`
  (add `--wide` too for HT40: `AIRDROP_HELPER_ARGS="--realtime --wide"`.)
- Read: `receive.log` KB/s and time; compare to run 8/9 (~580-660 KB/s). Payload airtime %:
  bigger % = the iPhone committed more.
- Prediction (if it works): airtime % and KB/s rise materially (toward MB/s). Refuted if unchanged — then the
  iPhone ignores a peer's real-time bit and true commitment needs the NAN data-path handshake (not buildable
  in-bounds); the honest ceiling is then a 2nd adapter. Either way this is the decisive test for approach 2.

### E0.11 run 9 orig design — **--wide: HT40 on 149** (opt-in, approach 2)
- Change: OWL `-H` advertises 40 MHz, helper `--wide` holds the GO at HT40+ on 5 GHz. Needs reinstall of BOTH
  (helper for `--wide`, OWL for `-H`):
  `pkexec install -Dm755 $PWD/helper/airdrop-helper /usr/lib/airdrop/airdrop-helper`
  `pkexec install -Dm755 -o root -g root $PWD/vendor/owl/build/daemon/owl /usr/local/lib/airdrop/owl`
- Run (149 is default; iPhone sends a large file, e.g. a short video):
  `AIRDROP_HELPER_ARGS="--wide" NAME=e011i DUR=300 scripts/airdrop-trace.sh --mode borrow`
- Read: `receive.log` KB/s, and MCS (HT40 shows the same MCS indices at ~2× Mb/s):
  `S=$(ls -d probe/sessions/*-e011i|tail -1); tcpdump -e -nn -r $S/air.pcap 'wlan addr2 02:00:00:00:00:07' 2>/dev/null | grep -oE "[0-9]+\.?[0-9]* Mb/s" | sort | uniq -c | sort -rn | head`
- Prediction: iPhone sends HT40 (MCS 15 at ~300 Mbit/s) ⇒ ~1.2 MB/s, a ~30 MB file completes. Refuted if the GO
  won't take HT40+ (warning) or the monitor misses the wider frames (rate/throughput unchanged → stay 20 MHz).
- Next after this: VHT80 on 149 (needs a VHT caps TLV in OWL, not built) for another ~2×.


- Finding (run 7 capture): while sending, the iPhone's sequence is **149-heavy** — 46 slots on 149 vs 28 on ch 6
  (`149,149,149,149,149,149,6,6,6,6,149,6,6,6,6,6`), because its Wi-Fi infra is 5 GHz so it keeps the data path on
  the 5 GHz social channel. We were on **ch 6 = its minority channel**, catching ~6/16 slots.
- The user's "follow the iPhone" idea: true per-slot hopping is impossible on this MT7921 (retune latency, E0.10),
  but we can sit on its *dominant* channel. **149 is clear here (country XX, not No-IR, 13 dBm, HT40+/VHT80).**
- No build needed (149 is an AWDL/GO channel; 2-stream OWL already installed). Run (iPhone sends the ~11.5 MB photo):
  `NAME=e011h DUR=300 scripts/airdrop-trace.sh --mode borrow --channel 149`
- Read: `receive.log` (SAVED? KB/s); `S=$(ls -d probe/sessions/*-e011h|tail -1); python3 scripts/awdl-decode.py $S/air.pcap --tlv 0x12 2>/dev/null | grep "seq: enc" | sed "s/.*enc=3 //;s/,/ /g" | tr " " "\n" | grep -E "^[0-9]+$" | sort -n | uniq -c | sort -rn` (does it give 149 even more slots when we are a 149 peer?); and on-air MCS as in run 7.
- Prediction: more slots caught (149 ~10/16) ⇒ higher duty cycle; 5 GHz MCS as good or better ⇒ effective rate up,
  the 11.5 MB file completes. Refuted if the GO on 149 in borrow mode behaves like E0.3b (0 frames) — but that was
  *beside a connected station*; here the station is paused, as in the working ch 6 GO.

### E0.11 run 7 (done above) — original command, for reference
- Change: OWL now advertises 2 streams + SGI20. **Reinstall OWL first:**
  `pkexec install -Dm755 -o root -g root $PWD/vendor/owl/build/daemon/owl /usr/local/lib/airdrop/owl`
- Run (iPhone sends a **~8-15 MB** photo to the laptop): `NAME=e011g DUR=300 scripts/airdrop-trace.sh --mode borrow`
- Read: `receive.log` (KB/s and SAVED?), and the on-air MCS:
  `S=$(ls -d probe/sessions/*-e011g|tail -1); tcpdump -e -nn -r $S/air.pcap 'wlan addr2 02:00:00:00:00:07' 2>/dev/null | grep -oE "MCS [0-9]+" | sort | uniq -c | sort -rn | head`
- Prediction: iPhone now uses **MCS 8-15** (was capped at 7); effective rate ~2×, a ~11.5 MB file may complete.
  Refuted if MCS stays ≤7 (then the iPhone ignores our 2-stream claim) or rate unchanged (duty-cycle bound, lever 3).

### E0.12 Do our injected frames air on ch 6? (visible test beacon) — **ready (now only if E0.7b run 3 fails)**
- One-off injector `scratchpad/beacon-inject.py IFACE SECONDS`: beacons "AIRDROP-TX-TEST" (TA = the vif's MAC,
  separate random BSSID) every 102 ms through the same vif OWL uses (`awdlact`). Observable by eye: the iPhone's
  Settings → Wi-Fi list. Not slot-dependent (an iPhone scanning sweeps all 2.4 GHz channels).
- (a) variant: during `airdrop receive --mode borrow` (GO). (b) control if (a) is negative: during a normal shared
  receive on ch 36 (known-good TX path) with `… IFACE SECONDS 36`.
- Prediction: (a) visible → TX works, the E0.11 failure is slot timing (1/16) → next: widen our sequence / E2.5.
  (a) invisible and (b) visible → injection does not air under borrow → next: different TX path.

### E0.5 iPhone as receiver: first send via `--go` — **moot** (GO refuted, E0.3b); replaced by E0.7c.

---

## E1 — BLE

### E1.4 Hash normalization and the 4th BLE hash (no radio; user types identifiers)
**Status: ready.**
- Question: (a) how are emails/phone numbers normalized before SHA-256 (owly OQ 1)? (b) which identifier gives the
  4th advert hash `dddd`?
- Known (measured, contact session): advert prefixes `aaaa` = record phone hash, `bbbb`/`cccc` = record email
  hashes, `dddd` = not in the record. Offline control (12:45): `dddd` is not SHA-256/SHA-1/SHA-512/MD5 of
  `altDsID`/`encDsID` (any case), nor a re-hash of the record hashes.
- Hypothesis: lowercase email and E.164 `+<digits>` phone, SHA-256 (as in PrivateDrop for older iOS); `dddd` is
  an identifier the account has that the record does not validate (e.g. an `@icloud.com` alias, or the Apple ID
  email).
- Variable: the candidate identifier × normalization rule. Control: the 3 known record hashes must match
  for the right rule.
- Steps (you type identifiers at a hidden prompt; only "candidate N: rule = match" is printed, nothing is stored):
  ```sh
  scripts/hash-check.py probe/sessions/20261103-001137-contact/dump/20261103-001138/001-Discover-SenderRecordData.der aaaa bbbb cccc dddd
  ```
  Candidates: each email of the Apple ID (Settings → your name → Sign-In & Security: all addresses, incl.
  @icloud.com/@me.com), and the phone number(s), typed as you normally write them.
- Confirms (a) if every record hash matches under one rule per type; (b) if some candidate gives `dddd`.
- **Run 1 (2026-11-04), partial.** Last prefix mistyped as `ddddcd` (so the 4th-hash check could not hit), 2
  candidates. Raw: candidate 1 (an email) → record email hash #1 (`bbbb`) under "as typed", "lowercase" and
  "local part without dots" (all three strings identical for that address, so the case/dot rule is not separated);
  candidate 2 (the phone number) → record phone hash #1 (`aaaa`) under **"digits only"**, not under "+digits" or
  "tel:+digits". Conclusion so far: hashes are plain SHA-256 (hex) of the identifier; phone = **digits only, no
  `+`** [M] (whether the country code is included depends on how it was typed: run 2). Open: email #2 (`cccc`), the
  email case rule, `dddd`.
- **Run 2 (2026-11-04), done.** Prefixes correct (`… dddd`), 6 candidates. Raw: (1) email #1 → `bbbb` under "as
  typed" *and* "lowercase" (so it was typed lowercase; case rule still not separated); (2) phone with `+`, country
  code, spaces → `aaaa` under "digits only"; (3) phone national, no country code → **no match**; (4) second Apple ID
  email → `cccc` (record email hash #2); (5), (6) other addresses → no match, and none of 1–6 gives `dddd` in any
  rule. Offline control: `dddd` ≠ SHA-256 prefix of "", device name, model, `SenderID`.
  **Conclusions [M]:** phone hash = SHA-256 of the **E.164 digits without `+`** (country code required, formatting
  ignored); emails = SHA-256 of the address; all 3 record hashes reproduced. `dddd` is not any identifier the user
  knows of on this Apple ID; remaining candidates: an identifier not listed in Settings (e.g. an old/secondary
  number), or a filler hash. Email case: run 3.
- **Run 3 (2026-11-04), done.** Candidate 3 = email #1 with a capital first letter → `bbbb` under "lowercase" but
  **not** "as typed" [M]. Conclusion: emails are **lowercased** before SHA-256. Dot handling is not testable with
  these addresses (none has a dot in the local part, so "without dots" = "lowercase"). Q-B3 answered.
  Script fix: Ctrl-C/EOF at the prompt now ends cleanly instead of a traceback.
- Run 2 design (kept): same command with `dddd` (no `cd`). Candidates, one variable each: (1) email #1 typed with
  its first letter **capitalized** (separates "as typed" from "lowercase"); (2) the phone number typed **with
  `+`, country code and spaces**; (3) the phone number typed **nationally without the country code**; (4) every
  other Apple ID address (Settings → your name → Sign-In & Security), including @icloud.com/@me.com/@mac.com.

---

## Static experiments (S-…)

Entries S-00…S-22 and S-24…S-26 answered questions by reading names and strings in the public iOS 27.2
firmware. They are not part of this repository. Their conclusions are in `docs/PROTOCOL.md`, tagged **[S]**,
and each one that matters for interoperability is paired with a measured ([M]) check there.

### C-1 Re-read of every recorded session (2026-11-16, no radio) — **done: Q-A1, Q-D1, Q-T3 answered; E2.2, E3.4 closed**

- **Question:** which open PROTOCOL §12 rows do the 85 sessions already on disk answer?
- **Setup:** two throwaway scripts over `probe/sessions/*/dump/*/*.json` (keys and values per message) and over every
  `air.pcap` through `scripts/awdl-decode.py`'s frame reader (TLVs per peer address, our own addresses from
  `iface.txt` left out). No prediction per row; each result is compared with the hypothesis PROTOCOL.md carried.
- **Results:**

| Row | Hypothesis in PROTOCOL.md | What the sessions show | Verdict |
|---|---|---|---|
| Q-A1 / E2.2 | TLV 0x21 bytes 4-5 = infra channel; 2.4 GHz gives `0b 51` | `24 81` (ch 36), `28 81` (ch 40), `0b 51` (ch 11: 663 frames, `-225836-rx-base`, `-230004-rx-vht80`), `64 80` (ch 100, a Mac), `00 00`; always equal to the DPS infra channel. Byte 9 is `00` on iPhones, `1a` on the Mac | confirmed |
| Q-D1 | reply unknown | `/Discover` reply = `ReceiverComputerName`, `IsAirDropable` true, `ReceiverMediaCapabilities` (1296 B JSON, identical 36/36); no `ReceiverModelName`. `/Ask` reply = name, model, `SupportsContactExchange` false, `IDSSessionID`, `ReceiverPseudonym`, `ReceiverPushToken` (23/23). `/Upload` reply = 200, empty (20/20) | answered |
| Q-T3 | unknown whether a sender must send `SenderIdentityAuthTag` | our `/Ask` has no tag, no record, no icon: 23 accepted | answered: not required (Everyone mode) |
| E3.4 | no HEIF declared → converted media | `{"Version": 1}` → JPEG, flat archive; HEIF + HEVC declared → `public.heic` in `NSIRD_AirDrop_*` folders with `._` sidecars | confirmed; "no NSIRD_ on iOS 27.2" **refuted** |
| Q-H4 | flags may follow device state | `111611` in 34 `/Discover` requests and 18 TXT records, two weeks, two sites | no change seen; E5.6 still the direct test |
| E2.7 | AWDL MAC rotates | `3a:b2:…` 11-02…11-05 (33 captures), `ea:bf:…` 11-08…11-16 (38); `ReceiverPseudonym` changed in the same gap | lasts days; trigger still open |
| TLVs > 0x18 | 0x1d-0x20, 0x22, 0x23 never sent | 0x20 (8 frames, two iPhones) and 0x22 (1 frame) in `20261112-222213-rx-guard` | corrected |

- **New:** a second request form. Two iPhones with AWDL Version `93 02` (`-223904-e011i`, `-225237-other-iphone`) send
  `/Discover` without `DeviceSupportFlags` and `/Ask` without `SenderComputerName` and `SenderIdentityAuthTag`.
- **E5.7 run of 2026-11-12 (`-175426-e57a`):** the iPhone was on AWDL for 18 s, answered 6 of 6 pings, had our
  record (its queries list it as a known answer) and still opened no TCP connection. Read again after C-2: the link
  was good, so this run **supports** "flags `0x1B3FB` make iOS not list us". One run; repeat with a control.
- **Changed:** PROTOCOL.md §3.1, §3.2, §5.2, §8.1, §8.3, §9.2, §10.3, §10.4, §10.5, §12, §13.

### C-2 Why "laptop" did not appear on 2026-11-16 17:08-17:13 (three runs, home) — **the iPhone's Wi-Fi state, not our flags**

- **Runs:** `-170843-e57a` (TXT flags `0x1B3FB`), `-171140-e57-control` (default `0x8B`), `-171252-e57b` (`0x3FB`).
  "laptop" was not listed in any of them, the control included, so the flags did not cause it.
- **What the traces show:** the iPhone is heard, acknowledges all our unicast frames at the radio level, but answers
  **0 of 38 pings**, never lists our record as a known answer and opens no TCP connection. Its channel sequence
  stays `36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36`: joined to the home network on ch 36, 2 of 16 slots on 149.
- **Across all sessions since 11-05** (ping replies / requests per session, from `awdl0.pcap`): with that ch 36
  sequence 2 of 10 sessions got replies (11-08 `send-nobbr`, 11-11 `-192203-rx-nowifi`) and 8 got none; with
  any other first sequence (infra channel 0, 11 or 40: `x,0,149,0,0,0,0,0,6,0,149,…` or 149-heavy) 34 of 37 did. Every fast run on
  record had the iPhone **off the ch 36 network**.
- **Corrects** the P2.5 note "an iPhone not joined to Wi-Fi gives 2/16 slots and resets: keep it joined". Those
  `rx-nowifi` runs still advertised infra channel 36; the failing state was "on ch 36", not "not joined".
- **Follow-up 17:25 (`-172521-e57-control2`, `-172542-e57c`), not conclusive.** The control worked at once (6.9 MB
  HEIC saved; 3/3 pings; the iPhone changed its sequence toward 149 one second after we appeared) **although its
  first sequence was still the ch 36 one**, so either the iPhone had not left the network or that state is not
  sufficient to fail. `e57c` (flags `0x1B3FB`, started 20 s later) had 0/15 pings and no known answer: the dead-link
  signature again, so it says nothing about the flags. Today at home: 1 of 5 sessions reached the iPhone. In every
  failing one the iPhone never adapts its sequence to us, i.e. it does not take us as an AWDL peer although it
  acknowledges our unicast frames. Our own action frames are not in `air.pcap` (the monitor does not record what
  we send), so whether they air in those sessions is not known.
- **Three plain repeats 17:30-17:31 (`-173014-nowifi-1`, `-173029-nowifi-2`, `-173106-nowifi-3`): 1 of 3.** The
  iPhone still advertised infra channel 36 in all three (the Control Center tap did not change that), so these do
  not test the Wi-Fi state. Run 1 connected in 4 s and saved a photo; runs 2 and 3, started 15 s and 37 s after the
  one before, had 0/8 pings each. Checked and ruled out: our radio's channel (every frame heard is on 5745 MHz in
  all eight sessions), the TXT flags (controls fail too), a stale share sheet (reopened).
- **Lead, not yet tested:** time since our previous session. The two sessions that worked today started 12 min and
  3.5 min after the one before; five of the six that failed started 15-90 s after one (the sixth, 17:08, was the
  first of the day at home). Each session uses a new random AWDL address under the same service name.
- `scripts/airdrop-trace.sh` now also records `tx.pcap` (what OWL sends, on `awdlact`); not yet tried on the radio.
- **Gap test 17:35 (`-173503-gap-long` 3.4 min after the last session, `-173541-gap-short` 8 s after): both failed
  (0/8 pings each).** The gap lead is **refuted**; back-to-back sessions also all worked on campus this
  morning (09:14-11:00, iPhone not joined to Wi-Fi).
- **`tx.pcap` works** and clears our side for these two sessions: 724 action frames spread over the whole period,
  60 multicast data frames of which 56 fall in the phase bin where the iPhone is heard on 149 (its bins 1 and 9 of
  16), unicast in the same bins and acknowledged; 802.11 headers and sequence numbers the same as in the working
  `-173014-nowifi-1`. In `gap-long` the iPhone even widened its sequence toward us (`36,0,149,0,0,149,36,36,…`), so
  it takes our action frames, yet sends **no unicast frame to us on 149** in either session.
- **Standing result:** iPhone joined to the home network on ch 36: 3 of 10 sessions today, 2 of 10 before (5 of 20).
  iPhone not on it: 34 of 37. Still without a direct test, because the iPhone advertised infra channel 36 in every
  run today. Hypothesis [H]: a joined iPhone answers a peer on its infra channel (36), where a radio pinned to 149
  cannot hear it.
- **Two plain `airdrop receive` runs 17:39 and 17:41 (no trace):** the first saved a photo at 1.5 MB/s with the iPhone
  still on the ch 36 sequence; the second never showed "laptop". Home total today: 4 of 12.
- **Checked on the two `tx.pcap` sessions and ruled out:** our slot timing (the next-slot time and AW number in our
  sync parameters agree with the iPhone's to 0.6 ms), our 802.11 headers, the gap between sessions.
- **Summary of the evidence (2026-11-16):** the failure belongs to one iPhone state, "joined to a 5 GHz network on
  ch 36": the iPhone gives 149 two slots of 16, acknowledges our frames there and sends us nothing (5 of 22
  sessions reach it). In every other state on record it works (34 of 37) and at 10-20 times the speed. The cause
  inside that state is **not found**. Next test (E2.11): put our radio on the iPhone's own channel instead,
  `--mode borrow --channel 36`, where it spends 7 slots of 16; never run with the current stack.
- **E2.11 run 1 (2026-11-16 17:50, `airdrop -v receive --accept-all --mode borrow --channel 36`, helper rebuilt to
  allow 36, iPhone joined to the ch 36 network): worked at once**, 6.9 MB in 2.1 s (3.3 MB/s at HT40; the GO on 36
  is not 80 MHz yet). One run. The user's requirement: it must also work when the iPhone is not on Wi-Fi, without
  choosing a channel by hand.
- **Built from that (not yet on the radio): the session follows the iPhone.** It starts on 149 as before. Every 2 s
  the helper reads the peer's channel sequence from OWL's log; when a channel it may run on (6, 36, 44, 149) has at
  least 4 of 16 slots and more than ours on two checks in a row, it tells the client (`{"state":"prefer",
  "channel":36}`), and `airdrop receive` restarts the link there once no request has come for 8 s (never during a
  transfer, at most every 20 s). On 36, 15 s without any peer sends it back to 149, where an iPhone that is not
  on the network can be heard. Replayed against today's OWL logs: every joined session asks for 36 within 2 s,
  no not-joined session asks for anything. Only with `--channel auto` and not `--mode shared`;
  `AIRDROP_NO_CHANNEL_MOVE=1` switches it off. Wi-Fi reconnects for a moment between the two links (v1).
  `send`, `find` and `listen` do not move yet.
- **First run of the channel-following build (17:55, plain `airdrop -v receive --accept-all`): saved a photo, on 149,
  before any move.** The helper log and OWL log of that session show the iPhone leaving its network 2 s after we
  came up (sequence `6,0,149,…` from 17:55:26), **19 s with nothing from it**, then rejoining at 17:55:45 and
  connecting within the same second. The user's report: "works only on Wi-Fi". So not-joined is not the safe
  state C-2 assumed either; no capture exists of a not-joined failure at home.
- **Session gap, looked at again over all 15 home runs today:** started less than 1 min after the previous
  session: 0 of 7 reached the iPhone; after 2.5 min or more: 5 of 8. Each session had a new random AWDL address
  under the same host and service name. **Change (built, not yet run): one AWDL address per boot**
  (`/run/airdrop-mac`), as an iPhone keeps its own for days. Hypothesis [H]: the iPhone holds the previous
  address for our name for a minute or two and drops data from the new one. Test: three sessions back to back.
- **Open:** why (it spends 7 slots on 36 and does not treat a 149-only peer as reachable?) and whether anything
  in our frames changes it: candidate for sitting D.

### C-3 What separates today's working and failing sessions (2026-11-16 18:10, no radio) — **the iPhone's AWDL state when we appear**

- **Runs added:** `-180019-mac-1`, `-180101-mac-2` (failed), `-180155-mac-3` (worked, 6.9 MB in 0.7 s), all with the
  per-boot address `02:00:00:00:00:09` and the iPhone off Wi-Fi (`6,0,149,…`). So the address was **not** the cause
  (back to back still 1 of 3), and neither is the iPhone's Wi-Fi state: C-2's two leads are both refuted.
- **Found:** the action-frame period in the iPhone's sync parameters (TLV 0x04 bytes 7-8) during the first 3 s we
  hear it. Today: **440 TU seen → worked, 3 of 3** (`e57-control2`, `nowifi-1`, `mac-3`); **only 110 TU → failed,
  12 of 12**. 440 TU is the iPhone's idle AWDL; 110 TU is AWDL already active. In the three that worked the
  share sheet opened 2.0-4.7 s after OWL started (first type-0x05 advert in `ble.btsnoop`). In the failures the
  iPhone was already active when we appeared: sheet open before us, or opened again so soon after the last
  session that its AWDL had not gone idle.
- **In the failing state** the iPhone advertises over Bluetooth the whole time (74-252 adverts), acknowledges our
  unicast, sometimes even asks `_airdrop._tcp` (4 queries in `gap-long`), and never takes our answers: no known
  answer for our record, no ping reply, no unicast to us.
- **History says this is new.** 11-11 and 11-12 have receive sessions that started in the 110 TU state and
  worked (`-163048-rx-ampdu`, `-164122-rx-p16`, `-192203-rx-nowifi`, `-005449-rx-guard2`). Since then the receive
  path gained: 80 MHz + VHT caps (API 10, 11-14), the short bring-up (API 9, 11-14), today's OWL build, today's
  driver build. **Next: bisect those with the failure reproduced on purpose** (share sheet open first, then start).
- **Bisect attempt 18:08-18:09 (`bis-base`, `bis-ht40`, `bis-careful`).** The failing state was not reproduced in two
  of the three (the sheet opened 3.0 s after OWL in `bis-base`; in `bis-careful` 1.8 s before OWL's first frame, and
  the iPhone was still at 440 TU). The predictor held again: 440 TU seen → worked (`bis-base`, `bis-careful`),
  110 TU only → failed (`bis-ht40`). Today's total: **5 of 5 and 13 of 13**. One valid bisect point: `--ht40`
  (no 80 MHz, no VHT caps) failed from the 110 TU state, so **80 MHz is not the cause**.
- **Second round 18:11-18:12 (`act-base`, `act-r54`, `act-old`, `act-oldslow`), sheet meant to stay open.** Three
  worked, back to back 12 s apart, with the sheet open 0.1-0.3 s around OWL's start; the iPhone was at 440 TU each
  time. `act-oldslow` (5.8 s bring-up, so our first frame came 3.1 s after the first advert) found it at 110 TU and
  failed. Predictor today: **440 TU seen → 8 of 8 worked; 110 TU only → 14 of 14 failed.**
- **Model after these runs [H]:** the iPhone is at 440 TU while idle and for roughly the first 2-3 s after the share
  sheet opens, then at 110 TU while AirDrop stays active (and for some time after). A peer whose first frames
  arrive in the 440 TU phase is taken into its data path; a peer that first appears in the 110 TU phase is
  acknowledged at the radio and otherwise ignored. Neither the gap between our sessions nor "sheet before us"
  predicts the outcome by itself; this does.
- **Bisect result so far:** from the 110 TU state, `--ht40` and `--ht40 --tx-rate 54 --careful` (the 11-12 radio
  settings and bring-up) both fail. So the settings are cleared; what is left against the 11-11/11-12 sessions that
  worked from 110 TU is the OWL build, the driver build and the Rust client/helper. Not yet bisected.
- **What it means in use:** a receiver that is already running when the sheet opens is in the good case. Every
  failure today was a session started while the iPhone was still active. `airdrop listen` starts a link per
  transfer and reaches the iPhone about 2.5 s after the sheet opens (1 s debounce + 1.4 s bring-up): at the edge.
  Next test: one long `airdrop receive`, several transfers in a row.
- **Long session 18:14 (`-181430-long`, one `receive` for 70 s, four shares): every share was found**, three photos
  saved, and a 417 MB video ran beside two of them (parallel `/Ask` + `/Upload` on separate connections work).
  So a receiver that stays up is reliable; C-3's model holds for the case that matters.
- **But the video ran at 3.0 MB/s** (125.8 MB in 43.7 s, steady) until it was stopped; the same phone did 21 MB/s on
  11-14. In this session the iPhone sent **no ADDBA request at all** (0 in `air.pcap`; every other connected
  session today has 3), so it sent one frame per transmission (gap 0.26 ms against 0.01 ms). It was the seventh
  connected session on the per-boot address `02:00:00:00:00:09`. Reading [H]: the iPhone asks for a Block Ack
  session up to three times per session and gives up on an address after enough of that; with a new address per
  session that never carried over. **The per-boot address is reverted** (random per session again). Open: why it
  asks three times when we answer each one.
- **After the revert (18:18 and 18:20, plain `airdrop -v receive --accept-all`, new address each time):** the iPhone asked
  for Block Ack again (2 requests in the first run, both answered) and the same 417 MB video arrived complete:
  **416.8 MB in 26.3 s = 15.8 MB/s** (3.0 MB/s on the reused address). The reading "an address reused over many
  sessions loses its Block Ack session" is confirmed by this pair; the mechanism inside the iPhone is not known.
  15.8 against 21.0 MB/s on 11-14: different file and day, not looked into.
- **Switched off by default:** channel following
  (`AIRDROP_CHANNEL_MOVE=1` turns it on); its premise, "joined to ch 36 is the failing state", does not hold.
- Timing inside the slot, slot numbering, 802.11 headers, the radio's channel: all checked again on these runs, all fine.

## Ready experiments (exact commands; run order = batches by place and iPhone setup)

**Re-based 2026-11-16.** The entries below were written when only shared mode on ch 36 worked. Every command still
runs as written: `scripts/airdrop-trace.sh` now uses the default mode (borrow on 149, Wi-Fi paused for the session
and reconnected), so read "shared mode on ch 36", "home" and "ch 6 site" as "anywhere, default mode". **iPhone
state for every run (C-2 below): not on the home ch 36 network** — Control Center → tap the Wi-Fi icon so it
disconnects (Wi-Fi itself stays on), wait 10 s. Joined to ch 36 it answered us in 2 of 10 sessions. Closed since:
E0.7a/b/c, E2.2, E3.4 (C-1 above).

Run order, one sitting each (the user runs them; one variable per run):

| Sitting | iPhone | Runs | Rows |
|---|---|---|---|
| A | Bluetooth only | E1.3, E1.5 | Q-B2, Q-B5 |
| B1 | sends one photo per run | E5.7 (first: it decides whether `/Hello` and `/Error` exist for us), E3.3, E5.4, E4.3 | Q-H1, Q-D3, Q-T1 |
| B2 | sends, failure paths | E5.1, E5.2, E5.3, E5.5 | Q-H2, Q-H3 |
| B3 | sends, one share type per run | E6.x | Q-P1 |
| C | receives (Everyone for 10 Minutes) | E5.6 through `find --dump` per iPhone state, E0.7d, E5.8, E5.9 (new, below) | Q-H4, Q-T2, Q-H2 |
| D | link layer | E2.6, E2.7, E2.5, E1.2, E3.2 | Q-A4, Q-A3, Q-B4, Q-D2 |


Every entry: question → prediction → one variable vs control → command → how to read it. Commands run from
`$PWD`. `trace` = `scripts/airdrop-trace.sh` (asks for sudo for the captures; Wi-Fi is never touched).
Knobs are environment variables read by the receiver/sender (no reinstall) or `AIRDROP_HELPER_ARGS` (needs the
reinstalled helper/OWL from 2026-11-04).

### Batch A — no radio or BLE only (anywhere)
- **E1.4** hash check — ready, above (Q-B1, Q-B3).
- **E1.3** byte 0 / tag vs AirDrop mode — ready, commands below (Q-B2 byte `0x40`). Read each with
  `sudo btmon -r $D/ble.btsnoop > $D/ble.txt; python3 -I scripts/ble-airdrop.py $D/ble.txt | grep -E "b0=|tag="`.
  ```sh
  cap() { D=probe/sessions/$(date +%Y%m%d-%H%M%S)-$1; mkdir -p $D; sudo -v; sudo btmon -w $D/ble.btsnoop >/dev/null &
          (printf 'menu scan\nduplicate-data on\nback\nscan le\n'; sleep 60) | bluetoothctl >/dev/null
          sudo pkill -f "btmon -w $D"; echo $D; }
  cap e1-3a   # share sheet open on a photo, AirDrop "Everyone for 10 Minutes" (control)
  cap e1-3b   # same, AirDrop "Contacts Only"
  cap e1-3c   # same, AirDrop "Receiving Off"
  cap e1-3d   # same, Wi-Fi off in Control Center
  cap e1-3e   # Bluetooth off/on first, then share sheet open
  ```
  Prediction: byte 0 `0x40` in a–d, tag changes only in e. Refuted if byte 0 or the hash set follows the mode.
- **E1.5** when is the 10-byte WPAWDL advert sent? (Q-B5). Control [M] 2026-11-04: **none** in the 7 sender-side
  btmon captures (`ble-airdrop.py --types` on `probe/sessions/*/ble.txt`). Variable: iPhone state, 60 s each,
  using `cap` above: `cap e1-5a` (iPhone idle on the home screen), `cap e1-5b` (Settings → General → AirDrop open),
  `cap e1-5c` (Control Center → long-press the connectivity tile → AirDrop menu open), `cap e1-5d` (iPhone Settings
  → General → AirDrop → "Bring Devices Together" toggled off then on). Read:
  `for D in probe/sessions/*-e1-5?; do sudo btmon -r $D/ble.btsnoop > $D/ble.txt; echo $D; python3 -I scripts/ble-airdrop.py $D/ble.txt --types | tail -4; done`.
  Prediction [H]: appears in none (it may be a Mac/AirPlay feature). Confirms a trigger if a `01 …` 10-byte item
  shows only in one state.

### Batch B — iPhone sends to us (P-SEND; default mode)
Each is one `trace` session; the iPhone shares the named item to "laptop". Control for all = a plain session with
the same item (the first line of each entry).
- **E3.4 HEIC conversion** (Q-D3, Q-P1). Use a photo the iPhone 17 took itself (Camera format "High Efficiency").
  (a) `NAME=e34a DUR=90 scripts/airdrop-trace.sh` (control, `{"Version":1}`);
  (b) `AIRDROP_MEDIA_CAPS=none NAME=e34b DUR=90 scripts/airdrop-trace.sh` (key omitted = Apple's "legacy" peer);
  (c) `AIRDROP_MEDIA_CAPS='{"Version":2,"ContainerFormats":{},"Codecs":{}}' NAME=e34c DUR=90 scripts/airdrop-trace.sh`
  (declares no HEIF container, no HEVC codec, S-16 key names);
  (d) after E0.7b: `AIRDROP_MEDIA_CAPS=$(ls probe/sessions/*-e07b/dump/*/*-Discover-reply-ReceiverMediaCapabilities.bin | head -1) NAME=e34d DUR=90 scripts/airdrop-trace.sh`
  (the iPhone's own blob = "supports everything").
  Read: `grep -h -E '"FileName"|"FileType"|ConvertMediaFormats' probe/sessions/*-e34?/dump/*/*-Ask.json`.
  Prediction: (a)/(d) send `public.heic` with `ConvertMediaFormats false`; (b) or (c) send a JPEG or set
  `ConvertMediaFormats true`. Refuted if all four are identical (then iOS 27 ignores the blob for this choice).
- **E3.3 TXT flags** (Q-D3). Share one JPEG photo. (a) `NAME=e33a DUR=90 scripts/airdrop-trace.sh` (0x8B);
  (b) `AIRDROP_TXT_FLAGS=0x3FB NAME=e33b DUR=90 scripts/airdrop-trace.sh`; (c) `AIRDROP_TXT_FLAGS=0x37FF NAME=e33c DUR=90 scripts/airdrop-trace.sh`.
  Read: `python3 -I scripts/dump-diff.py probe/sessions/*-e33?/dump/*`. Prediction: `/Ask` keys unchanged except
  possibly `ConvertMediaFormats`/`UsePKZip`; refuted if nothing changes (TXT flags ignored by iOS 27 senders).
- **E3.2 mDNS backoff** (Q-D2). `NAME=e32 DUR=200 scripts/airdrop-trace.sh`; open the share sheet once on a photo
  and leave it open 3 min **without tapping laptop**. Read:
  `tcpdump -nn -tt -r probe/sessions/*-e32/awdl0.pcap 'udp port 5353' 2>/dev/null | grep -v 1613:33ff | grep -c ""` and the
  gap list `… | awk '{print $1}' | awk 'NR>1{printf "%.3f\n",$1-p}{p=$1}'`. Prediction: gaps = multiples of 1.048 s,
  growing (1, 2, 4, … s) then capped.
- **E4.3 client cert under TLS 1.3** (Q-T1). Share a photo:
  `AIRDROP_TLS_PROBE=13 AIRDROP_TLS_CA=probe/sessions/20261103-001657-tlsprobe/certs/client-62865-cert1.der NAME=e43 DUR=90 scripts/airdrop-trace.sh`.
  Read: `grep -h -E 'version|peer_certificate' probe/sessions/*-e43/dump/*/tls-*.json`. Prediction: TLS 1.3, a peer
  certificate on the `/Ask` connection (same Apple ID cert as R-08), none on `/Discover`. Refuted: no certificate
  (then iOS sends it only under 1.2) or the transfer fails at the handshake (read `receive.log`).
- **E5.1 decline** (Q-H2). `AIRDROP_ASK_STATUS=403 NAME=e51 DUR=60 scripts/airdrop-trace.sh`, share a photo; note
  the iPhone's text. Read: `ls probe/sessions/*-e51/dump/*/` (any request after `/Ask`, e.g. an error message) and
  `tcpdump -nn -tt -r probe/sessions/*-e51/awdl0.pcap 'tcp[tcpflags] & (tcp-fin|tcp-rst) != 0'`. Prediction: iPhone
  shows "declined", sends nothing more, closes with FIN.
- **E5.2 /Ask timeout** (Q-H2). `AIRDROP_ASK_DELAY=20 NAME=e52a DUR=90 scripts/airdrop-trace.sh`, then
  `AIRDROP_ASK_DELAY=40 NAME=e52b DUR=120 scripts/airdrop-trace.sh`; share a photo each time. Read the FIN/RST time
  minus the `/Ask` time (same tcpdump as E5.1; `/Ask` time in `*-Ask.json`). Prediction: iOS gives up at a fixed
  time (owly: 15 s receiver timeout? then both fail at the same offset).
- **E5.3 cancel mid-upload** (Q-H2). `NAME=e53 DUR=150 scripts/airdrop-trace.sh`; share a long video (≥ 200 MB) and
  tap Cancel on the iPhone at about half. Read `receive.log` (last progress line), `ls dump/*/` and the FIN/RST
  tcpdump. Prediction: TCP close without any HTTP error message; we delete the partial file.
- **E5.4 contact exchange** (Q-H1). `AIRDROP_CONTACT_EXCHANGE=1 NAME=e54 DUR=120 scripts/airdrop-trace.sh`; share a
  photo; afterwards watch the iPhone for a contact-exchange offer. Read: `ls probe/sessions/*-e54/dump/*/` — any
  request other than Discover/Ask/Upload (e.g. `NNN-<Path>.json`, answered 400) gives the path and keys.
  Prediction: an extra POST with `GivenName`/`FullName`/`Handle`/`VCardData` (path = its name). Refuted: nothing
  extra (then the exchange needs more than the flag, e.g. a contact relationship).
  **Revised 2026-11-12 (S-26):** sharingd skips the exchange when "Not needed {senderContact, alwaysExchangeContacts,
  type}"; it is the NameDrop flow (near-field tap). Expect nothing for a photo; a run is still cheap.
- **E5.7 Does the iPhone send `/Hello` when we advertise its TXT flags?** (Q-H1, Q-H4). Hypothesis (S-26): "Receiver
  does not support HELLO, skipping" is decided by a TXT `flags` bit we don't set. Run on the home setup, iPhone
  sends a photo each time:
  `AIRDROP_TXT_FLAGS=0x1B3FB NAME=e57a DUR=90 scripts/airdrop-trace.sh` (the iPhone's own flags). Read:
  `ls probe/sessions/*-e57a/dump/*/` — a `NNN-<Path>.json` other than Discover/Ask/Upload (answered 400, which the
  sender tolerates) gives the Hello path and its `FeatureFlags`/`ContextType` values. If it appears, bisect the
  unnamed bits (0x10, 0x20, 0x100, 0x1000, 0x2000, 0x8000, 0x10000) with `e57b…`. Refuted: no extra path with the
  full flag set (then Hello needs a known/paired peer). Watch: 0x0004 is not in 0x1B3FB, so pipelining stays off.
  The 2026-11-12 run was void (C-1). **If a new path appears** (here or in E5.4), run once more with
  `AIRDROP_UNKNOWN_STATUS=200` added: the receiver then answers it 200 with an empty plist, and the dump shows what
  the iPhone sends next (prediction for Hello: it goes on to `/Ask` as before; an `/Error` path becomes possible
  on a cancel, so repeat E5.3 in that state).
- **E5.5 resume** (Q-H3). `NAME=e55a DUR=200 scripts/airdrop-trace.sh`; share a large video (≥ 500 MB); press
  Ctrl-C in the terminal at about 50 %; at once `NAME=e55b DUR=200 scripts/airdrop-trace.sh` and share the same
  video again. Read: `python3 -I scripts/dump-diff.py probe/sessions/*-e55?/dump/*` (TransferID, TotalBytes, any
  range/offset key). Prediction: new TransferID, full upload, no resume key.
- **E5.6 `DeviceSupportFlags` vs iPhone state** (Q-H4). Share a Safari link (fast) once per state, 60 s each:
  (a) control `NAME=e56a DUR=60 scripts/airdrop-trace.sh`; (b) Low Power Mode on → `NAME=e56b …`; (c) iPhone's own
  AirDrop "Receiving Off" → `NAME=e56c …`; (d) Settings → General → AirDrop → "Use Mobile Data" off → `NAME=e56d …`;
  (e) "Bring Devices Together" off → `NAME=e56e …` (same `DUR=60 scripts/airdrop-trace.sh` each).
  Read: `grep -h -o '"DeviceSupportFlags": [0-9]*' probe/sessions/*-e56?/dump/*/*Discover*.json`. Prediction: some
  bits change with state (SDStatusMonitor); refuted if `111611` in all (then the bits are static capabilities).
- **E6.x share matrix** (Q-P1). One session per item, `NAME=e6-<item> DUR=90 scripts/airdrop-trace.sh`, items: live
  (Live Photo), multi (3 photos), album, folder (Files), pdf, note, maps (a place), wallet (a pass), contactphoto
  (contact with photo), big (> 4 GB file from Files), youtube, music, appstore (links). Read:
  `python3 -I scripts/dump-diff.py probe/sessions/*-e6-*/dump/*` and `ls probe/sessions/*-e6-*/dump/*/`.
- **E2.6 election** (Q-A4). Needs the reinstalled helper + OWL. Share a photo:
  `AIRDROP_HELPER_ARGS="--election-metric 500" NAME=e26 DUR=90 scripts/airdrop-trace.sh`; control = any earlier
  session (metric 60). Read: `python3 scripts/awdl-decode.py probe/sessions/*-e26/air.pcap --tlv 0x18 | head` (whose
  address is master) and whether the transfer still completes. Prediction: the iPhone (65) accepts us (500) as
  master and follows our sequence.

- **E2.7 AWDL MAC rotation trigger** (PROTOCOL §3.1). Two photo shares 2 min apart: (a) `NAME=e27a DUR=60 scripts/airdrop-trace.sh`;
  then on the iPhone toggle Airplane Mode on/off; (b) `NAME=e27b DUR=60 scripts/airdrop-trace.sh`. Read the iPhone's
  AWDL source address in each: `python3 scripts/awdl-decode.py probe/sessions/*-e27a/air.pcap | head -3` (same for b).
  Prediction: new MAC after the toggle (rotation on AWDL restart); refuted if unchanged.

- **E2.8 Single-band DPS** (after the first ch 6 transfer works). Q: does the iPhone give a ch 6 peer more slots
  when that peer says it is 2.4 GHz only? OWL advertises DPS flags `0x8f24`, which include 0x0020 *dual band*,
  although in borrow mode our radio is pinned to ch 6 (a dual-band peer invites traffic on 149 [H]). Variable:
  `AIRDROP_HELPER_ARGS=--dps-single-band` (OWL `-S`: flags `0x8f04`) vs control (default). Same share both times
  (iPhone sends one photo): `NAME=e28a DUR=120 scripts/airdrop-trace.sh --mode borrow`, then
  `AIRDROP_HELPER_ARGS=--dps-single-band NAME=e28b DUR=120 scripts/airdrop-trace.sh --mode borrow`.
  Read: iPhone sequences `python3 scripts/awdl-decode.py probe/sessions/*-e28?/air.pcap --tlv 0x12` (ch 6 slots
  during the upload) and transfer time in `receive.log`. Prediction: more ch 6 slots / faster with `-S`.

- **E2.9 Infra channel in DPS** (after the first ch 6 transfer works). Q: does the iPhone put more slots on ch 6
  for a peer that advertises infra channel 6? Variable: `AIRDROP_HELPER_ARGS=--dps-infra-channel` (OWL `-C 6`:
  DPS flags `0x8f25`, BSSID `00…00`, channel `06 00` — the same layout the iPhone uses for its own infra channel;
  harness-checked bytes `0c 17 00 25 8f 58 30 00 01 00 00×6 06 00 <mac> 00 00`). Control: default. Commands as
  E2.8 with `NAME=e29a/e29b`. Read the same way (ch 6 slots during the upload, transfer time).

### Batch C — iPhone receives (P-RECV; default mode)
- **E0.7a/b/c** — ready, above (Q-D1, first send). By-products: E2.2 (Q-A1 byte check), E2.4 (Q-A3), E2.6 control.
- **E1.2 wake variants** (Q-B4). Now possible at home (borrow mode). Evidence so far [M] (e07b2): our v3 zero-hash
  advert enabled 17:50:47.082 (HCI), the receiving iPhone probed + announced `_airdrop._tcp` at 17:50:48.160
  (+1.08 s); e07b the same ~1 s after AWDL up. Consistent with a BLE wake, but no control yet. Control = E0.7b
  (v3 zero-hash advert). (b) `AIRDROP_HELPER_ARGS="--ble-version 1" NAME=e12b DUR=60 scripts/airdrop-trace.sh find -t 45`;
  (c) `AIRDROP_HELPER_ARGS="--ble-version off" NAME=e12c DUR=60 scripts/airdrop-trace.sh find -t 45`. Read: found or
  not, and the iPhone's AWDL frame count `python3 scripts/awdl-decode.py probe/sessions/*-e12?/air.pcap | head -3`.
- **E5.6 through `find`** (Q-H4), cheaper than the sender-side version above: the iPhone's TXT `flags` is the same
  bitmask. Per iPhone state (control; Low Power Mode; "Bring Devices Together" off; Wi-Fi off, Bluetooth on):
  `NAME=e56r-<state> DUR=40 scripts/airdrop-trace.sh find -t 30`. Read:
  `grep -h -o '"flags": "[0-9]*"' probe/sessions/*-e56r-*/dump/*/*mdns.json`. Prediction: `111611` in all (C-1 saw no
  change in two weeks); a bit that moves names itself by the state that moved it.
- **E0.7d Contacts Only, what a stranger sees** (Q-T2). iPhone AirDrop set to **Contacts Only**, screen on:
  `NAME=e07d DUR=40 scripts/airdrop-trace.sh find -t 30`. Read: found or not; if a record exists, its TXT and whether
  `/Discover` is answered and with which keys (`ls probe/sessions/*-e07d/dump/*/`). Prediction: it is announced over
  mDNS but the `/Discover` reply has no `ReceiverComputerName` (so it is not listed), or the connection is closed.
  Observation only: we send nothing we do not send in Everyone mode.
- **E5.8 the sender stops mid-upload** (Q-H2, receiver side). iPhone on Everyone, accept the prompt:
  `AIRDROP_SEND_CANCEL_AT=30 NAME=e58 DUR=90 scripts/airdrop-trace.sh send -r testphone BIGFILE` (≥ 200 MB). Note what
  the iPhone shows and whether a partial file appears in Photos/Files. Read `send.log` and the FIN/RST lines of
  `awdl0.pcap`. Prediction: "Transfer Failed" on the iPhone, nothing saved, no HTTP reply to us.
- **E5.9 the upload starts late** (Q-H2). `AIRDROP_SEND_UPLOAD_DELAY=20 NAME=e59a DUR=90 scripts/airdrop-trace.sh send -r testphone FILE`,
  then `=45` as `e59b`; accept at once each time. Read `send.log` (completed or "upload interrupted") and the time
  of the iPhone's FIN/RST after its `/Ask` reply. Prediction: the iPhone waits a fixed time for the body; both
  complete if that time is over 45 s, else both fail at the same offset.
- **E2.5 widen/narrow** (Q-A3). `AIRDROP_HELPER_ARGS=--intersect NAME=e25 DUR=60 scripts/airdrop-trace.sh find -t 45`
  vs E0.7b. Read `--tlv 0x12` (iPhone sequence) and whether find still works.

### Closed by static + existing measurement
- E2.3 (DPS flags vs phase): closed by S-20 + R-05.


### P2.1 Send-speed analysis, no radio (2026-11-08) — session `20261105-115624-send1` (2.5 MB in 21.6 s)
- Install state OK: installed helper and OWL md5 equal the repo builds; the installed `airdrop` is newer than its sources. The "auto != explicit" gap is likely already closed (auto passes `--mode borrow`; `--wide`/`--realtime` are default no-ops there). Verify with one live auto run in the soak.
- `awdl0.pcap` TCP: 1804 data segments x 1378 B. Upload is **steady 100-180 KB/s every second** from t=10 s to t=31 s, with no long gaps. Receive is bursty duty-cycle bound. Send behaves like a **paced trickle** (~100 segments/s), not an airtime wall.
- First ~10 s is handshake/ARP-like delay: the iPhone's SYN-ACK is retransmitted several times before our ACK is accepted; gaps of 3.7 s, 3.1 s and 2.3 s early on.
- Hypotheses to test, in order: (1) OWL TX gate or queue releases ~1 frame per slot-window for unicast; (2) injected-frame rate/aggregation (no A-MPDU, AWDL-15 ADDBA unanswered); (3) our advertised window (win 74 x scale) vs iPhone ACK clocking. Next: count our TX frames per 16-slot cycle from `air.pcap` (tcpdump has no tshark here; use `scripts/awdl-decode.py`) and compare with `owl.log` queue lines.
- **P2.1 result (2026-11-08): send is TCP-loss/RTT limited, not airtime-limited.** From `awdl0.pcap`: 26 retransmits of 1802 data segments (1.4 %), 225 dup-ACKs of 1296 ACKs (17 %), median in-flight 22 KB (~16 segments). Rate 130 KB/s / 22 KB in flight => effective RTT ~170 ms (iPhone ACKs arrive in per-cycle bursts). Mathis estimate MSS/RTT*1.22/sqrt(p) with p=1.4 % gives ~80 KB/s, matching the 90-116 KB/s measured. Host has only reno/cubic allowed (`tcp_available_congestion_control`).
- Levers (client/helper side, no auth involved): (a) BBR on the awdl0 route (`ip -6 route replace fe80::/64 dev awdl0 congctl bbr`, helper does it after `modprobe tcp_bbr`; route dies with awdl0 so teardown stays fail-safe); (b) raise `tcp_reordering`/disable early-retransmit if dup-ACKs are reordering from batched delivery rather than loss; (c) AWDL-15 ADDBA to cut MAC-level loss. Test (a) first.
- **P2.1 control (2026-11-08 12:14, `--no-bbr`, borrow 149):** 2.5 MB in 39.4 s (63.7 KB/s); 46 retransmits / 1793 segments (2.6 %), 562 dup-ACKs / 1434 (39 %), median in-flight 36 KB. Two earlier BBR runs (12:11, 12:12) never connected: SYNs to the iPhone's :8770 got no reply for 25-30 s and the iPhone stayed at 2/16 slots on 149 (never widened as on 11-05). Cause: iPhone not freshly in receive mode (the control worked after toggling Receiving Off -> Everyone for 10 Minutes), not BBR (SYN precedes congestion control). Lesson for the soak: re-arm "Everyone for 10 Minutes" before every run.
- **P2.1 BBR route bug (2026-11-08):** `ip -6 route replace fe80::/64 dev awdl0 congctl bbr` added a second route (metric 1024) next to the kernel's `proto kernel metric 256` one, so BBR was never active in the 12:11/12:12/12:16 runs (see `iface.txt`). Fixed to `metric 256 proto kernel congctl bbr`. Those three runs also never connected (no SYN-ACK from the iPhone for 25-60 s, vs 7 ms in the 12:14 control), so "iPhone not reachable on a given run" is a separate, frequent failure: 3 of 4 runs today. Next step is the soak harness plus a /Discover-reachability metric, not BBR.
- **P2.1 BBR verdict (2026-11-08 12:44, session `20261108-124450-send-bbr3`):** BBR route confirmed in `iface.txt` (`fe80::/64 proto kernel metric 256 congctl bbr`). 2.5 MB in 33.7 s (74.5 KB/s) — inside the no-BBR spread (90, 116, 64 KB/s). In-flight rose to 51 KB but speed did not: 73 retransmits / 1867 segments (3.9 %), 1135 dup-ACKs / 1695 (67 %). **TCP congestion control is not the bottleneck; frames are being lost or delayed on the air.** Hypothesis "cwnd/loss-limited TCP" FALSIFIED as the main cause. Helper flag inverted: BBR is now opt-in (`--bbr`), default off.
- Lead (from `vendor/owl/src/tx.c:314`): OWL's TX radiotap header carries only RATE = 6 Mbit/s legacy OFDM (12 x 500 kbit/s), no MCS, no TX flags. The ratefix module may override it, so first check what rate the frames actually air at. Candidate levers: radiotap MCS (HT40 MCS 15) for unicast data to a peer that advertised HT caps; per-peer retry behaviour; the 67 % dup-ACK rate says the iPhone sees holes, i.e. MAC-level loss at a conservative rate is not the cause but missed/overrun slots might be.
- **P2.1 finding (2026-11-08):** `scripts/mt7921-ratefix.sh status` says the **stock** mt7921e/common are loaded during all of today's sends (kernel 7.2.5). OWL asks for 6 Mbit/s OFDM only; stock may honour or ignore it. Next A/B: load the per-kernel ratefix build and repeat the same 2.5 MB send (Wi-Fi blips ~3 s during the swap, reverts on reboot or `revert`).
- **P2.1 ratefix A/B (2026-11-08 12:47, session `20261108-124748-send-ratefix`):** rate-fixed mt7921e loaded (phy index moved phy0 -> phy1; helper coped). 2.5 MB in **17.9 s (140.4 KB/s)** vs stock 33.7-39.4 s (64-75 KB/s) and 21.6-27.7 s (90-116 KB/s) on 11-05. Confound: the installed helper still had BBR on by default (reinstall pending for the opt-in change), so this run = ratefix + BBR. Best send number so far, single run; needs repeats via the soak harness before concluding.


### P2.2 What bounds send speed? (2026-11-11, no radio; `scripts/analyze-trace.py` on the 11-08 sessions) — **answered: our PHY rate**
- Method: the monitor capture (`air.pcap`) contains the TX-status copy of every frame we inject (radiotap TX flags:
  bit 0 = not acknowledged). Grouping full-size data frames into bursts (gaps < 5 ms) gives the cost of one frame on air.
- Result [M]:

  | session | driver | frame gap in a burst | = PHY rate | duty | MAC loss | KB/s |
  |---|---|---|---|---|---|---|
  | `20261108-121426-send-nobbr` | stock | 2.33 ms | ~6 Mbit/s | 7.9 % | 1.8 % | 63.5 |
  | `20261108-124450-send-bbr3` | stock | 2.34 ms | ~6 Mbit/s | 9.5 % | 3.2 % | 74.2 |
  | `20261108-124748-send-ratefix` | rate-fixed | 1.09 ms | 12 Mbit/s | 8.7 % | 1.7 % | 139.7 |

- Reading: inside a burst frames leave back to back, so one frame costs exactly its airtime. OWL asked for 12 Mbit/s
  (`ieee80211_radiotap_rate_to_val(12)` is 24 half-Mbit units; the 11-08 note "6 Mbit/s" misread it). The rate-fixed
  driver honours that; the stock driver ignores it and uses ~6 Mbit/s under the GO. That alone is the 2x of the
  ratefix A/B, so **the 11-08 result is explained by the rate, not by BBR** (the confound is resolved).
- MAC loss is 2-3 %: the link is clean. The iPhone sends to us at MCS 12-15 HT40 on the same path, so there is
  plenty of SNR for a faster legacy rate.
- **Change:** OWL `-r <rate>` sets the rate of unicast data frames (`6..54` Mbit/s, or `mcsN[-40][-sgi]` for a driver
  that honours HT injection; ours does not yet). Action frames and multicast stay at 12 Mbit/s. Helper default:
  **24 Mbit/s on 5 GHz** (a mandatory OFDM rate), `--tx-rate N` to choose. Prediction: frame gap ~0.65 ms at 24
  (about 1.7x), ~0.40 ms at 54 (about 2.7x), if MAC loss stays low. Needs the rate-fixed driver loaded.
- Not pursued: HT-rate injection in the driver. HT20 MCS 7 is 65 Mbit/s (no gain over 54), and at MCS 15 HT40 the
  fixed per-frame overhead (~0.2 ms) dominates, so the extra gain over 54 Mbit/s is ~1.7x for a driver change.
  Revisit only after the 54 Mbit/s run is measured.
- **Ready [LIVE]** (iPhone: Receiving Off, then Everyone for 10 Minutes, before each run):
  `scripts/mt7921-ratefix.sh load`, then
  `NAME=send-r24 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone ~/Downloads/AirDrop/IMG_1571.jpg` and
  `AIRDROP_HELPER_ARGS="--tx-rate 54" NAME=send-r54 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone ~/Downloads/AirDrop/IMG_1571.jpg`.
  Read: `python3 -I scripts/analyze-trace.py probe/sessions/*-send-r24 probe/sessions/*-send-r54` (frame gap, MAC loss, KB/s).
  If 54 holds MAC loss under ~10 %, make it the default.

### P2.3 What bounds receive speed? (2026-11-11, no radio) — **answered: per-frame overhead; aggregation is refused by us**
- Result [M] (`analyze-trace.py`): `20261105-102119-e210a` 31.2 MB: 16.5 % duty, frame gap 0.30 ms;
  `20261105-010303-e210a` 44.5 MB: 17.3 % duty, frame gap 0.31 ms; `20261104-223904-e011i`: 17 %, 0.30 ms.
- **Correction to the "2-4 % duty cycle" figure** (E0.11 run 9, E2.10): that number divided bytes by the 300 Mbit/s
  PHY rate. Measured on air, the iPhone sends to us **16-20 % of the time**. A 1500-byte frame at 300 Mbit/s is
  40 us of payload, but each one is sent alone: contention + preamble + SIFS + ACK make it ~0.24 ms, and every second
  one triggers a TCP ACK frame from us (~0.26 ms at 12 Mbit/s). So in-burst throughput is only ~3.3 MB/s.
- The iPhone asks us for a Block Ack session every 1-2 s during an upload (ADDBA request: TID 0, immediate, window 64,
  timeout 58000; 13-38 requests per session, addr3 = AWDL BSSID). We never answered, so it never aggregates:
  0 A-MPDU frames in every capture. owly's AWDL-15 and its MT7612U note (30 MB in 14 s instead of 47 s with Block Ack
  accepted) say this is worth ~3x where the chip answers A-MPDUs itself.
- **Change:** OWL `-A` answers the request (status 0, immediate Block Ack, requested TID, no A-MSDU in A-MPDU, window
  <= 64, requested timeout); helper `--ampdu`, **off by default**. Checked by replaying `send-bbr3/air.pcap` through
  the patched library: 32 requests seen, 32 well-formed answers.
- Open [LIVE] question: does the MT7921 firmware answer an A-MPDU with a Block Ack in this setup (active monitor
  beside a hidden GO, peer not a station of the GO)? If not, the iPhone gets no acknowledgement for aggregates and the
  transfer gets slower or fails, hence opt-in. **Ready:**
  `AIRDROP_HELPER_ARGS="--ampdu" NAME=rx-ampdu DUR=180 scripts/airdrop-trace.sh --mode borrow` (iPhone sends the 31 MB
  video), then `python3 -I scripts/analyze-trace.py probe/sessions/*-rx-ampdu`. Confirms: "Block Acks sent by us" > 0,
  frame gap well under 0.3 ms, KB/s up. Refutes: 0 Block Acks sent and a slower or failed transfer (then leave it off).
- Smaller lever, also from this: our TCP ACK frames at 24 Mbit/s instead of 12 (comes with P2.2; predicted ~5 %).
- Idea not built: thinning our pure TCP ACKs in OWL (1 per 4 segments instead of 1 per 2) would give the iPhone
  roughly 25 % more of each burst. Only worth trying if `--ampdu` is refuted.

### P1.1 Why 3 of 4 sends on 11-08 never connected (2026-11-11, no radio) — **answered: AirDrop on the iPhone was not receiving**
- Result [M] (`analyze-trace.py`, sessions `20261108-121108-send-bbr`, `-121637-send-bbr2`): the iPhone's radio
  **acknowledged** 27 and 30 of our TCP SYNs to port 8770 and 9 and 11 of our ICMPv6 echo requests, and sent back
  nothing at all: 0 TCP frames, 0 echo replies. In the working runs it answers pings and the SYN within milliseconds.
  It stayed at 2/16 slots on channel 149 instead of widening to 5-7/16.
- Reading: the frames arrived, so this is not a radio, channel, rate or BBR problem. The iPhone's AWDL interface was up
  for another reason but AirDrop was not listening: "Everyone for 10 Minutes" had run out (the 12:14 control worked
  right after re-arming it). Our zero-hash BLE advert only wakes an Everyone receiver.
- **Change:** `airdrop send/find` now tell the two failures apart. "N Apple device(s) in radio range but did not answer:
  set Everyone for 10 Minutes again" when AWDL peers were seen and none answered `/Discover`, versus "No Apple device
  was heard on channel N" when there were none. The stale channel-6 hint is gone. `analyze-trace.py` prints the same
  verdict from a capture ("reachable" line).

### Stability changes (2026-11-11), all covered by loopback QA tests (12/12 green)
- **Ctrl-C crash** (`20261105-115401-send1`: `RuntimeError: no running event loop`, `Invalid file descriptor`): a second
  Ctrl-C during the several-second teardown raised KeyboardInterrupt and closed the event loop under the cleanup. The CLI
  now handles SIGINT and SIGTERM itself: the first cancels the command, later ones print "Still stopping (restoring
  Wi-Fi)". Exit code 130.
- **Stalled transfers**: a receiver whose sender goes silent (locked, out of range) waited forever with a partial file;
  a sender whose receiver vanished waited for TCP to give up (~15 min). Both now stop after 60 s without progress
  (`AIRDROP_STALL_TIMEOUT`), the receiver removes the partial file and keeps serving.
- **Connect retries when sending**: `/Discover` and the `/Ask` connection are each tried 3 times (the iPhone leaves our
  channel for 10-14 s at a time), instead of failing on the first 10 s timeout.
- **Honest send progress**: the 4 MiB socket buffer made a 2.5 MB send show 100 % at once and then sit; now 256 KiB.
- **Version checks**: the helper reports `HELPER_API`; the client and `airdrop doctor` say when the installed helper or
  OWL is older than the client, with the install command. `doctor` also checks that channel 149 may transmit and which
  mt7921e build is loaded. OWL logs one `traffic:` line per 5 s (sent, acked, lost, received, block-ack requests).



### P2.2 / P2.3 live results (2026-11-11 16:27-16:31, campus, borrow on 149, rate-fixed driver, iPhone 17) — **both confirmed**
| run | session | result | frame gap | duty | MAC loss |
|---|---|---|---|---|---|
| send, `-r 24` | `20261111-162712-send-r24` | 2.5 MB in 6.4 s = **390 KB/s** | 0.58 ms | 8.0 % | 0.4 % |
| send, `-r 54` | `20261111-162823-send-r54` | 2.5 MB in 3.7 s = **676 KB/s** | 0.32 ms | 5.9 % | 0.3 % |
| receive, `-A` | `20261111-163048-rx-ampdu` | 43.4 MB in 13.5 s = **3.2 MB/s** | 0.04 ms | 11.7 % | 0.3 % |

- Send: predictions held (0.65 / 0.40 ms predicted). Against 64-140 KB/s before: **4.8x at 54 Mbit/s** over the best
  earlier run. Single runs each; loss did not rise with the rate.
- Receive: the MT7921 **does** answer A-MPDUs under the GO. 3 requests, 3 answered, 30477 of 31774 frames arrived
  aggregated, 2.25x the 1.43 MB/s of E2.10b on the same 43.4 MB video. "0 Block Acks sent by us" in the analyser is
  expected: the chip's own Block Acks do not appear on its monitor interface.
- **Defaults changed (helper, HELPER_API 3):** 54 Mbit/s on 5 GHz (`--tx-rate 24` for a far device; no rate fallback
  exists); Block Ack accepted in borrow mode (`--no-ampdu` to switch off; shared mode still opt-in, untested).
- **Bug found and fixed in the same session:** with many access points around, the helper's channel check failed
  borrow mode twice (`20261111-162904`, `-162934`: "the radio is on 2462 MHz"). NetworkManager's scan after the
  disconnect filled the 20-frame sample with other channels. Under the GO the channel is confirmed by
  `iw dev awdlgo info`, so hearing 5745 MHz at all now counts; otherwise a warning, not an abort.
- Open: repeat runs for a spread; larger sends; whether the early NetworkManager scans cost airtime; receive with
  the stock driver (TCP ACKs then go out at ~6 Mbit/s).

### P2.4 What bounds the two fast runs now? (2026-11-11 evening, no radio; sessions `-163048-rx-ampdu`, `-162823-send-r54`) — **answered: our replies wait behind OWL's slot gate**
- Ruled out [M]: OWL's receive path (air -> awdl0 lag median 0.6 ms, max 2.2 ms, 31774 of 31774 frames delivered);
  air loss (192 sequence holes = 193 TCP retransmits, 0.6 %); our receive window (674 KB-10 MB against bursts of
  60 KB-2.3 MB); the iPhone being away (in the idle gaps it MAC-acks our small frames: 57 acked, 5 lost).
- Found [M], receive, steady phase: each iPhone visit opens with ~130 aggregated frames in 6 ms, then **nothing for
  33-43 ms** until our first TCP ACK leaves; after that data and ACKs alternate every ~2 ms for 40-110 ms; then
  ~130 ms idle and the same again (cycle ~230 ms). Example (ms): data x129 at 451.5, our ACKs at 490.0; data x137 at
  667.7, our ACKs at 717.7. The kernel produced those ACKs within 1 ms; they waited in OWL (`awdl_can_send_unicast_in`).
- Why the gate is late: slot edges come from software timestamps of the master's sync parameters, and both
  sequences are snapshots (the iPhone switched 5/16 -> 9/16 -> 11/16 -> 5/16 within one transfer; ours follows on
  the 1 s peer-table timer).
- **Change:** OWL `-p MS` (presence window). A peer heard within the last MS ms is on our channel, so unicast to it
  goes out at once; a received frame also re-runs the unicast sender instead of waiting for its timer. Helper default
  **16 ms in borrow mode** (`--presence 0` off). The `traffic:` log line counts frames "sent on presence".
- Prediction: the 33-43 ms opening wait disappears (~110 ms visits become fully used, about +40 % receive), and the
  ramp-up shortens (7 of the 11 s of the 43 MB transfer were TCP slow start with one round trip per visit).
  Unknown: whether the ~130 ms idle between visits is the iPhone's real absence or the same gate at the far end.
- Also removed: two of the three `/Discover` connections before a send (the search's answer is reused for 30 s; the
  mDNS and direct paths no longer probe the same device at once). In `send-r54` they cost ~3 s before `/Ask`.
- **Ready [LIVE]** (reinstall OWL + helper first): the same 43 MB video with
  `NAME=rx-p16 DUR=180 scripts/airdrop-trace.sh --mode borrow`, and
  `NAME=send-p16 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone "$HOME/Downloads/AirDrop/IMG_1606 (1).MOV"`
  (first send above 2.5 MB). Read with `analyze-trace.py`; compare duty and KB/s with 11.7 % / 3215 and 5.9 % / 676,
  and `grep traffic: probe/sessions/*-rx-p16/owl.log` for "sent on presence".

### L1 Background listener: can Bluetooth tell us when to pause Wi-Fi? (2026-11-11) — **detection confirmed; full loop ready [LIVE]**
- Idea (user, 2026-11-11): keep Wi-Fi connected, watch Bluetooth only, pause Wi-Fi just for a transfer.
- Old captures [M] (`ble.btsnoop` of 14 receive sessions, 11-04..11-11): the sending iPhone's type-0x05 advert was in
  13 of 14 (the miss was another phone, "iPhone"), 3-5 adverts/s for as long as the share sheet is open, always the
  same hash set `bbbb dddd cccc aaaa`. With the laptop already up, the first `/Discover` came 0.04-4.3 s after the
  first advert. Bring-up (script start -> OWL running) took 7-9 s (17 s once).
- Live, Wi-Fi **connected** on channel 100 (`airdrop listen --dry-run`, unprivileged `bluetoothctl` scan) [M]:
  2 of 2 openings seen (16:49:01, 16:49:15); the second ran 26 s with 36 adverts. The first lasted 0.7 s / 3 adverts
  (the sheet only flashed), so the listener wants an advert to persist 1.2 s before it acts.
- **Built:** `airdrop listen`. Wi-Fi is left alone until an AirDrop advert persists 1.2 s; then borrow-mode bring-up
  with the helper's new `--fast` waits (0.5 s instead of 2 s before the GO with retries, no frame-listening check,
  0.4 s OWL settle), mDNS, the normal receiver with the y/n/always prompt. Wi-Fi comes back 3 s after a saved
  transfer, 25 s after the sheet closed with nothing sent, or after 90 s without any request. One session per
  opening of the sheet. Timing lines are printed: trigger -> visible, visible -> found, reconnect time, total pause.
- **For now it reacts to ANY iPhone nearby (user's choice for testing, 2026-11-11).** In a public place a stranger
  opening AirDrop pauses Wi-Fi for ~25 s. To restrict it later: `--hash bbbb --hash dddd --hash cccc --hash aaaa`
  (the user's iPhone; one matching hash is enough). Nothing is saved without the prompt unless `--accept-all`.
- Open: each trigger runs pkexec. Without the polkit rule (`packaging/50-airdrop-background.rules.example`) that is
  a password dialog once the few-minute `auth_admin_keep` window has passed, which makes the laptop appear late.
- Not yet measured: trigger -> visible with `--fast`; whether the fast GO start is reliable; reconnect time.

### P2.4 live result (2026-11-11 16:41, `20261111-164122-rx-p16`, `-p 16`) — **confirmed**
- Same 43.4 MB video: **8.6 s = 5.0 MB/s** (13.5 s / 3.2 MB/s without the presence window; 30.3 s on 11-05).
  31804 frames aggregated, MAC loss 0.2 %. OWL `traffic:` lines: 240 and 105 frames "sent on presence" in the two
  busy 5 s windows, i.e. frames the slot gate would have held. The send run (`send-p16`) was not done.

### L1 live results (2026-11-11 16:59-17:02, `airdrop listen`, helper log `/run/airdrop-helper.log`) — **the loop works**
- Two triggered sessions. Helper bring-up with `--fast`: 16:59:39 -> up 16:59:41 and 17:00:55 -> up 17:00:56,
  **1-2 s** (7-15 s with the normal waits the same afternoon). Six items (5 photos + the 43 MB video) saved in the
  second one.
- Giving Wi-Fi back: `nmcli connection up` took 4-5 s on the campus 802.1X network (1-6 s over the day).
- Bugs found and fixed:
  1. The six items landed in six `PHMediaFormatConversionRequest-<UUID>/` folders. Photos' staging folders are now
     flattened like `NSIRD_` (QA test added).
  2. Ctrl-C during teardown killed the helper's `nmcli connection up` ("terminated by signal Interrupt"; the fallback
     `nmcli device connect` then reconnected). Helper children now run in their own session.
- Faster to appear (untested on the radio): mDNS registration without the 3 x 500 ms conflict probing; no IPv6
  duplicate-address detection on `awdl0` (OWL writes `accept_dad=0` before bringing it up; the address was unusable
  for 1-2 s); debounce 1.2 -> 1.0 s; Wi-Fi back 1.5 s after a save instead of 3 s.
- **Listener defaults (user, 2026-11-11): any iPhone triggers it, and in the terminal everything is accepted without
  asking.** `--ask` brings the y/n/always question back; it becomes the default when the shell integration supplies
  its own prompt. `--hash` restricts who triggers it.
- Reconnect time is NetworkManager's (association, 802.1X, DHCP); nothing on our side waits before it starts.

### L1 run 3 (2026-11-11 17:19, `airdrop listen`, helper API 6, no trace) — **loop timed end to end**
- Trigger 17:19:43 -> visible **1.7 s** -> first request from the iPhone **4.3 s** later -> 6 items, 64.7 MB in 36.5 s
  (1.8 MB/s) -> Wi-Fi back in **2.7 s**; paused 60.1 s in total. Items saved flat (the folder fix holds).
- Why 4.3 s to be found: OWL log shows the iPhone at 2/16 slots on 149 until its first contact (11/16 from 17:19:49).
  One announcement round every 3 s rarely falls into those two slots. **Change:** announce every ~0.3 s for the
  first 12 s after appearing, then every 3 s as before. Untested.
- Why 1.8 MB/s and not 5: OWL `traffic:` lines per 5 s show 626, 3149, 2597, 8987, 4358, 8138, 7734, 18177 data frames,
  so only the last window ran at the single-video rate (5 MB/s). Hypothesis: the iPhone converts each photo to JPEG
  while sending (the items come from `PHMediaFormatConversionRequest` folders) and that paces the stream; our side
  shows no loss (0-3 lost frames per window). **Change, opt-in:** `airdrop config set media original` makes
  `/Discover` advertise HEIF/HEVC support (`ReceiverMediaCapabilities` Version 3, the container and codec part of the
  iPhone's own blob from `send-r54/dump/…/002-Discover-reply-ReceiverMediaCapabilities.bin`). Prediction: `.HEIC`
  files, no conversion folders, higher and steadier rate. This is the old E3.4 with a concrete blob. Undo:
  `airdrop config set media compatible`.
- Small fixes: leftover characters on the progress line; "Still stopping" printed on a single Ctrl-C.

### L1 other people's iPhones (2026-11-12, no radio) — **one cause fixed, untested; symptom still needed**
- Report (user): `airdrop listen` works only with the user's iPhone.
- Checked: nothing selects a phone. No `--hash` in use, config has no list, receiver / mDNS / `/Discover` reply are
  the same for every sender.
- Found and fixed: the listener kept ONE opening for all phones together. A second iPhone opening its sheet while
  another's adverts were still in the air (or within 8 s of them) was never a new opening, so nothing was triggered.
  Openings are now tracked per iPhone (by hash set); binary rebuilt.
- Not ruled out [LIVE], in order: (1) the other iPhone is not joined to a Wi-Fi network (2/16 slots on 149, known to
  reset) or is on 2.4 GHz Wi-Fi (its busy channel is 6, we sit on 149); (2) its advert is not heard or parsed
  (`airdrop listen --dry-run` shows this without touching Wi-Fi); (3) an older iOS with a different upload format.

### P2.5 Where is receive bound after the presence window? (2026-11-11, no radio; `20261111-164122-rx-p16`) — **answered: the iPhone's own presence**
- Steady phase [M]: while the iPhone is sending, data and our ACKs alternate with no waits (43 aggregated frames per
  ~2.2 ms = **~27 MB/s**). The opening wait of P2.4 is gone (`data x125` at 0.0 ms, our ACKs at 6.0 ms).
- The visits are short: e.g. 62 ms on, 111 ms silent, 45 ms on, 187 ms silent. At TCP level, in 16 of 19 silences
  everything the iPhone had sent was already acknowledged, our window was 16 MB, and it resumed with new data; 3
  followed a lost frame (SACK). So in the silences nothing is owed by us: the iPhone is either off our channel or not
  producing data. Its advertised sequence (149 x6, 6 x3, 149 x5, 6 x2) would allow far longer visits than it makes.
- Conclusion: on our side receive is at its limit; the remaining factor (~30 % presence) is the iPhone's.
- Hypothesis to test [LIVE], no code: the iPhone was joined to the campus Wi-Fi (channel 100) and has to serve
  it between visits. Same 43 MB video with the iPhone's Wi-Fi **disconnected from the network** (Control Centre: tap
  the Wi-Fi icon so it turns white; Wi-Fi itself stays on, AirDrop needs it): `NAME=rx-nowifi DUR=180
  scripts/airdrop-trace.sh --mode borrow`. Confirms: duty well above 11 % and more than 5 MB/s.

### P2.6 Send beyond 54 Mbit/s: HT rates in the driver (2026-11-11) — **built, not loaded, untested**
- Why: at 54 Mbit/s one frame costs 0.32 ms, so sending tops out near 4.3 MB/s while the iPhone is present, against
  the ~27 MB/s it reaches towards us. Legacy rates end at 54.
- Built: `vendor/mt76-driver/mt7921e-ratefix-ht-7.2.5.ko` (patch
  `vendor/mt76-driver/patch/mt7921-family-driver-ht.diff`): an injected frame that asks for an HT rate (radiotap MCS
  0-15, 40 MHz, short GI) gets that fixed rate in the TX descriptor, encoded as mt7915's test mode does (mode HT,
  index = MCS, NSS = MCS/8). Only `mt7921e_tx_prepare_skb` changed; imported symbols identical to the proven
  rate-fixed module; vermagic matches. OWL already takes `-r mcsN[-40][-sgi]`.
- Unknown: whether the firmware sends a fixed HT rate to a peer that is not a station of our hidden access point,
  and whether the iPhone acknowledges it. A wrong guess shows as frames that never get acknowledged (send fails or
  crawls); reloading the normal build or a reboot undoes it.
- **Ready [LIVE]** (iPhone: Receiving Off, then Everyone for 10 Minutes, before each):
  1. Baseline, also the missing send run of P2.4:
     `NAME=send-54 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone "$HOME/Downloads/AirDrop/IMG_1606.MOV"`
  2. `HT=1 scripts/mt7921-ratefix.sh load`, then
     `AIRDROP_HELPER_ARGS="--tx-rate mcs7" NAME=send-mcs7 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone "$HOME/Downloads/AirDrop/IMG_1606.MOV"`
  3. `AIRDROP_HELPER_ARGS="--tx-rate mcs15-40-sgi" NAME=send-mcs15 DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone "$HOME/Downloads/AirDrop/IMG_1606.MOV"`
  Read: `python3 -I scripts/analyze-trace.py probe/sessions/*-send-54 probe/sessions/*-send-mcs7 probe/sessions/*-send-mcs15`.
  Confirms: frame gap under 0.32 ms with MAC loss still low. Refutes: gap ~2.3 ms (rate ignored) or high loss.
- If confirmed, the next step is bundling several packets per frame (A-MSDU, which an AWDL node must accept): at
  MCS 15 the fixed per-frame overhead dominates, and bundling five packets would bring the cost per packet from
  ~0.2 ms to under 0.1 ms. Not built; it only pays once HT rates work.


### P2.5 live result (2026-11-11 19:22, home, `20261111-192203-rx-nowifi`) — **refuted; the test itself fails**
- With the iPhone disconnected from its Wi-Fi network the 43 MB video did not arrive: `/Ask`, `/Upload`, then
  "Connection reset by peer", twice. The iPhone advertised `36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36` for the whole
  session: **2/16 slots on our channel** and it never raised that (joined to Wi-Fi it goes to 11/16 within 3 s).
  14666 data frames in 55 s. Sessions `-192142`, `-192338`, `-192428` are empty starts of the same attempt.
- So an iPhone without a network is the worse case for us, not the better one. Do not repeat. The silences this
  experiment was meant to explain are ours: see P2.7.

### P2.6 live results (2026-11-11 19:25-19:28, home, borrow on 149, HT driver build loaded for runs 2-3) — **HT rates work**
| run | session | result | cost of one frame in a burst | duty | MAC loss |
|---|---|---|---|---|---|
| `-r 54` (baseline) | `20261111-192517-send-54` | 43.4 MB in 41.6 s = **1.04 MB/s** | 0.32-0.35 ms | 22.8 % | 0.2 % |
| `-r mcs7` | `20261111-192644-send-mcs7` | 43.4 MB in 38.7 s = **1.12 MB/s** | 0.28 ms | 20.9 % | 0.2 % |
| `-r mcs15-40-sgi` | `20261111-192803-send-mcs15` | 43.4 MB in 22.7 s = **1.91 MB/s** | **0.16 ms** (522 frames in 84 ms) | 18.5 % | 0.1 % |

- The firmware sends the fixed HT rate and the iPhone acknowledges it: at MCS 15/40/SGI a burst carries ~8.5 MB/s
  (54 Mbit/s: ~4 MB/s), loss unchanged. MCS 7 = 54 Mbit/s as predicted. The analyser's "frame gap" (a median) reads
  0.26 ms for MCS 15 because TX status arrives in pairs; frames per burst length is the honest figure.
- The averages are a quarter of that because we transmit only ~20 % of the time (P2.7). The 2.5 MB sends of the
  afternoon (676 KB/s) were too short to show it.
- OWL logged "unable to inject packet (Resource temporarily unavailable)" 24 / 1 / 45 times; TCP resent 106 / 86 /
  83 segments of 31.5k. Small; not the bound.

### P2.7 Why are we silent 70 % of a transfer? (2026-11-12, no radio; the three sends above + `-164122-rx-p16`) — **our radio is scanning**
- Folding data-frame times onto the 16 x 64 TU schedule reproduces the iPhone's advertised sequence exactly (on 6.5
  slots, off 2.5, on 5.5, off 1.5), and inside one 64 TU slot the frames are spread evenly. So the schedule is
  followed, and the silences do not come from it.
- Inside the slots where the iPhone is on 149, data flows for at most ~108 ms and then stops for ~135 ms (cycle
  215-242 ms; 123 silences = 15.4 of 21.3 s in `send-mcs15`, 234 = 25.1 of 36.0 s in `send-54`). Data was queued in
  all but one; nothing of ours completes inside them.
- Frames from **other channels** appear in the monitor capture only inside those silences, never while data flows
  (send-54: 2412-2472, 5180, 5240, 5765 MHz in 24 silences; send-mcs15: 15), and in sweep order
  2412 → 2472 → 5180 → 5240 → (quiet DFS channels) → 5765, **repeating every ~9 s for the whole session**. ~38
  channels in 9 s = 237 ms per channel = the measured cycle: ~130 ms on the scanned channel, ~105 ms back on 149.
  A silence on a channel without access points shows no frames, which is why only a fifth of them are "proven".
- So while wlan0 is disconnected (borrow mode) something scans back to back and the radio is on 149 for ~45 % of
  the time; times the iPhone's 11/16 that is the ~20-30 % duty of every fast run so far. **P2.5's "the iPhone's own
  presence" was this** (`rx-p16` has the same off-channel frames), and the receive side is not at its limit.
- Not known: who asks (NetworkManager, wpa_supplicant, or the desktop shell's network widget; nothing in the
  journal). `iw event` for 25 s with Wi-Fi connected (11-12): no scan at all, so it belongs to the disconnected state.
- **Change:** helper `--scan-guard` (opt-in, borrow mode): listens to `iw event` and stops every scan on our phy as
  it starts (`scan abort`, `scan sched_stop` for a scheduled one); it ends before teardown reconnects Wi-Fi. The
  first five are logged in `/run/airdrop-helper.log`, the total at teardown. `airdrop-trace.sh` now records
  `iw-event.log`; `analyze-trace.py` prints an "off channel" line (frames heard on other channels, scans started /
  aborted).
- Unknown: whether the mt7921 firmware honours an abort quickly, and whether the requester simply asks again at
  once (then the guard needs a different handle: the requester itself).
- **Ready [LIVE]** (reinstall the helper first; HT build loaded; iPhone joined to Wi-Fi, Receiving Off then
  Everyone for 10 Minutes before each):
  1. `AIRDROP_HELPER_ARGS="--tx-rate mcs15-40-sgi --scan-guard" NAME=send-guard DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone "$HOME/Downloads/AirDrop/IMG_1606.MOV"`
  2. `AIRDROP_HELPER_ARGS="--scan-guard" NAME=rx-guard DUR=180 scripts/airdrop-trace.sh --mode borrow` (the same video from the iPhone)
  Read: `python3 -I scripts/analyze-trace.py probe/sessions/*-send-guard probe/sessions/*-rx-guard; grep -h "scan" probe/sessions/*-guard/iw-event.log | head -20; grep "scan guard" /run/airdrop-helper.log`.
  Confirms: duty above 40 %, send well above 1.9 MB/s, receive above 5 MB/s, "off channel" near 0. If the scans
  keep running, `iw-event.log` shows on which interface and how often, which names the requester.
- A-MSDU bundling (P2.6) stays behind this: the radio's absence costs more than the per-frame overhead.

### P2.7 live result (2026-11-12 00:38, home, HT build, `--tx-rate mcs15-40-sgi --scan-guard`) — **send confirmed; receive not run**
- `20261112-003832-send-guard`: 43.4 MB in 15.1 s = **2.87 MB/s** (1.91 without the guard, 1.04 at 54 Mbit/s).
  Every scan that started under the guard was aborted within 5-16 ms (`iw-event.log`; rc=0), 1 off-channel frame in
  the whole transfer (158-178 before). The requester asks again after 5, 7, 10, 14, 20 s (a back-off, so
  NetworkManager's periodic scan is the likely source); each attempt costs a few ms.
- The last 7 s ran at 2750 frames/s = **3.8 MB/s with 57 % duty**; the iPhone's schedule allows 69 % (11/16), and
  one frame costs 0.205 ms in a burst, so ~4.6 MB/s is the ceiling of this path until packets are bundled (A-MSDU).
- The first 6 s ran at half that (30 % duty, no off-channel frames): TCP slow start overran OWL's injection socket
  (103 x "unable to inject ... Resource temporarily unavailable" in 2 s, 102 segments resent) and the connection
  spent seconds recovering. **Fixed:** `wlan_send` waits for room in the socket (poll, up to 40 ms) and no longer drops.
- A scan that was already running when the guard started (NetworkManager's after the disconnect) ran to its end,
  9 s into the session. **Fixed:** the guard aborts a running scan when it starts.
- **The guard is now the default in borrow mode** (`--no-scan-guard` to switch off).
- `20261112-003918-rx-guard` is not a result: the iPhone never started a send (0 AirDrop Bluetooth adverts against
  122 in a working session, no `_airdrop` query, no connection; it answered our pings for 105 s). Repeat it.
- **Ready [LIVE]** (reinstall helper and OWL; HT build loaded; iPhone joined to Wi-Fi):
  1. `AIRDROP_HELPER_ARGS="--tx-rate mcs15-40-sgi" NAME=send-hold DUR=120 scripts/airdrop-trace.sh send --mode borrow -r iphone "$HOME/Downloads/AirDrop/IMG_1606.MOV"`
     Confirms the injection fix: no "unable to inject" in `owl.log`, the whole transfer near 3.8 MB/s (~11-12 s).
  2. `NAME=rx-guard DUR=180 scripts/airdrop-trace.sh --mode borrow`, then send the 43 MB video from the iPhone.
     Compare with 5.0 MB/s / 8.6 s (`rx-p16`).

### P2.7 / P2.6 live results, second round (2026-11-12 00:54, home, scan guard default, OWL holds frames) — **both directions confirmed**
| run | session | result | before |
|---|---|---|---|
| send, `--tx-rate mcs15-40-sgi` | `20261112-005411-send-hold` | 43.4 MB in 7.7 s = **5.6 MB/s** | 2.87 (guard only), 1.91 (HT only), 1.04 (54 Mbit/s) |
| receive (6 HEIC + 1 MOV) | `20261112-005449-rx-guard2` | 58.4 MB in 4.8 s = **12.2 MB/s** | 5.0 (`rx-p16`) |

- Send: 0 "unable to inject", 0 off-channel frames, MAC loss 0.1 %; the scan already running at start was aborted.
  The client's 7.7 s includes the handshake; data frames flowed 41 % of the transfer span.
- Receive: 43028 of 42979+ frames aggregated, 0 off-channel frames. The analyser's 18.6 % duty counts only
  back-to-back time at 0.04 ms per frame; the transfer is no longer bound by our radio's absence.
- **Bug found:** the seven items arrived complete but were saved as two folders, `Containers/` and `Media/`: with
  items from two staging areas the archive carries the iPhone's own trees
  (`Containers/Data/PluginKitPlugin/<UUID>/tmp/NSIRD_AirDrop_x/IMG.HEIC`,
  `Media/PhotoData/OutgoingTemp/PHMediaFormatConversionRequest-<UUID>/IMG.MOV`) and only a staging folder at the top
  was stripped. **Fixed:** the extractor takes each item's `FileBomPath` from `/Ask`, keeps the item and what is
  inside it, and skips the folders above it (`cpio.Extractor(roots=...)`). Test
  `test_iphone_sends_photos_and_a_video_from_different_staging_trees` replays the layout; pytest 14/14. The frozen
  `airdrop` binary was rebuilt.
- Open: HT rate as a default (the helper cannot tell that the HT module is loaded); A-MSDU on send; the same runs at
  the campus (many access points, 802.1X reconnect after aborted scans).

### Defaults after the 2026-11-12 runs (built, **not yet on the radio**) — plain `airdrop send` / `receive` should now be the fast path
- **OWL `-r auto` / `-r auto-ht`:** starts at the fastest rate of a ladder (HT: MCS 15, 13, 11 at 40 MHz short GI,
  then 24 and 12 Mbit/s; legacy: 54, 36, 24, 12) and steps down when more than 10 of 100 frames go unacknowledged,
  back up after 4-32 clean windows. Windows older than 2 s are dropped (idle pings to an absent peer). Simulated
  only: stays on MCS 15 at 0.2 % loss, falls to 24 Mbit/s when HT fails, returns to MCS 15 when clean. Each change
  is one line in `owl.log` ("unicast data rate now ...").
- **Helper (HELPER_API 7):** on 5 GHz the default rate is `auto-ht` when the HT driver build is loaded, else `auto`
  (`--tx-rate` still fixes one). It knows the build from `/run/airdrop-mt7921-ht` (root-owned; holds the module's
  srcversion; written by `scripts/mt7921-ratefix.sh load`, gone at reboot like the module). With the stock driver
  it prints one notice. Scan guard on by default (above).
- **`scripts/mt7921-ratefix.sh load`** now loads the HT build when one exists for the running kernel (`HT=0` for the
  plain one). `airdrop doctor` tells the three driver states apart and flags an OWL without `-r auto`.
- **Ready [LIVE]** after `pkexec install ... airdrop-helper`, `scripts/build-owl.sh --install`,
  `scripts/mt7921-ratefix.sh load` (writes the note; once per boot): plain
  `airdrop send -r iphone FILE` and `airdrop receive`. Confirms: `grep "data rate" /run/airdrop-owl.log` shows
  "automatic, starting at HT MCS 15" and no step down at arm's length; speeds as in the second round. Then the same
  from another room: the rate should step down and the transfer still complete.

### First plain runs with the new defaults (2026-11-12 01:25, home) — **two faults found, both fixed**
- `airdrop send`: 43.4 MB in 16.5 s = 2.6 MB/s. OWL ran `-r auto` ("starting at 54 Mbit/s"), not `auto-ht`: the
  note in `/run` did not exist (`mt7921-ratefix.sh load` was not run after the change), so the helper took the
  loaded HT build for a plain one. **Fixed:** the helper knows the HT builds' srcversion itself
  (`HT_DRIVER_SRCVERSIONS`; add the value after a rebuild for another kernel); the note stays as a second way.
  HELPER_API 8. For the record, 54 Mbit/s with the scan guard is 2.6 MB/s (1.04 before it).
- `airdrop receive` (7 items, 58.4 MB, with the accept question): "Declined", then a second attempt that sat at
  99.8 % until Ctrl-C. The progress line redraws the terminal line the question was on, so "Accept?" was never
  visible; at 11 MB/s the whole upload is spooled before anyone answers, and the second attempt was simply waiting
  for an answer. **Fixed:** no progress line while the question is open; a bare Enter asks again (it used to mean
  no). Test `test_enter_alone_does_not_decline`; pytest 15/15; binary rebuilt.
- Still unconfirmed on the radio: `auto-ht` (and its step-down from another room), and the question flow.

### Rust CLI + Rust helper send (2026-11-13 22:25, home ch 36 → borrow 149, `20261113-222500-rust-send`) — **confirmed**
- `scripts/airdrop-trace.sh send --mode borrow -r iphone IMG_0688.MOV` with `AIRDROP=target/release/airdrop` and
  `AIRDROP_HELPER=target/release/airdrop-helper`: **132.6 MB in 24.2 s = 5.5 MB/s**, `Sent`, and Wi-Fi was restored.
- `analyze-trace.py`: 60.5 % duty (the iPhone allows 69 %, 11/16 slots), frame gap 0.28 ms over 96,489 full
  frames, MAC loss 0.1 %, 0 off-channel frames, 6 of 8 scans aborted. This is the same frame gap and loss as the Python
  client's best (`20261112-005411-send-hold`, 5.6 MB/s on 43 MB, 41 % duty): the Rust sender is at parity.
- **`-r auto-ht` is now confirmed as the default** (HELPER_API 7/8): "automatic, starting at HT MCS 15, 40 MHz,
  short GI", no step down, loss 20-31 per ~20,000 frames.
- The `-v` line "did not answer /Discover: ... port 8770: Connection refused" is the direct probe to the
  iPhone's AWDL address on the fixed port. The iPhone listens on the port it gives in mDNS, so the probe is refused,
  and the iPhone is found through mDNS anyway. It is harmless.

### A-MSDU on send (P2.6) — **refuted on this card by analysis; not built**
- **The iPhone never sends A-MSDU on AWDL.** In `20261112-005449-rx-guard2` (12.2 MB/s) all 43,036 data frames
  from it are QoS Data with the A-MSDU bit clear, 1.0-1.5 KB each, carried in A-MPDUs. Our send uses plain Data
  frames (96,531 in `rust-send`), one TCP segment each.
- **The frame size is capped below two segments.** `ip -d link show wlan0` gives `maxmtu 2304`; mt76 sets no
  `hw->max_mtu`, so mac80211's default applies to the monitor vifs too, and AF_PACKET injection is bounded by the vif
  MTU. One full AWDL segment is ~1440 B of IPv6 plus 14 + 8 + 8 B of A-MSDU subframe, LLC/SNAP and AWDL headers. Two
  do not fit in 2304, so an A-MSDU would carry one full segment and nothing for a bulk send.
- Going past that needs a driver patch (`hw->max_mtu`). Even then, two things are unknown: whether the firmware sends
  an injected 3.8-7.9 KB frame, and whether the iPhone's AWDL path accepts A-MSDU at all (it never uses it).
- **What is left for send:** 60.5 % of the 69 % the iPhone allows, at 0.28 ms per 1.4 KB frame (mostly preamble,
  SIFS/ACK and backoff: the payload is ~40 µs of it at MCS 15/40). Our own injected frames cannot be aggregated
  (A-MPDU is built by the firmware only for its own station entries). On one radio, send is near this card's ceiling.

### Bring-up latency, HELPER_API 9 (2026-11-14) — **confirmed: start 5.7 s → 1.4-1.6 s**
- Question: how much of a `receive`/`send` start is fixed waiting, now that the scan guard exists?
- Before (API 8, plain `receive`/`send`): disconnect, **2 s** settle "for NetworkManager's scan", GO (one try),
  monitor pair with up to **3 s** "hears frames", **1-9 s** frame check on the channel, scan guard, OWL (1 s
  settle), resolvectl, BLE. The 2 s and the frame check existed only because NM's post-disconnect scan dominated the
  sample (2026-11-11 campus: 2462 MHz x19 against 5745 MHz x1).
- After (API 9 default): scan guard right after the disconnect (that scan is aborted at once), 500 ms settle (the
  value `listen --fast` has used live since 11-11; the regulatory domain can still change after the disconnect),
  GO start retried while busy, no frame check and no hearing wait in borrow mode, OWL 0.4 s, BLE before resolvectl.
  Reconnect activates the paused connection on the access point it was on first (`nm_disconnected.ap`).
- Every session logs `timing: up in N s (...)` and `timing: restore in N s (...)` in `/run/airdrop-helper.log`.
- Run (iPhone sends the 43.4 MB video each time; A first, then B):
  ```sh
  AIRDROP_HELPER_ARGS=--careful target/release/airdrop -v receive   # A: API 8 sequence
  target/release/airdrop -v receive                                 # B: API 9
  grep timing: /run/airdrop-helper.log | tail -4
  ```
- Prediction: B's `up` is 3-10 s shorter than A's, transfer speed unchanged (10-12 MB/s), Wi-Fi back in each.
  Refuted if: B's start fails or retries (`ap start attempt`), the radio is off channel during the transfer
  (`analyze-trace.py` "off channel"), or speed drops. The reconnect lever: refuted if `restore`'s "reconnect wi-fi"
  is not shorter than in A.
- **Result (2026-11-14, home, Wi-Fi 'Sofia' on 2.4 GHz):** A `up in 5.68 s` (settle 2000 ms, monitor pair 1006 ms,
  channel check 1205 ms, OWL 1153 ms); B `up in 1.59 s` (settle 500, monitor pair 36, check 0, OWL 558). Ten more
  API 9/10 starts since: 1.39-1.58 s, none retried. Speed unchanged (11.1 vs 10.6 MB/s on 43.4 MB). **Confirmed.**
- **Reconnect** (not separable by `--careful`, which keeps the access-point path): it came out at either ~0.75 s or
  ~3.0 s. NetworkManager's journal gave two causes:
  1. Switching device auto-connect on first made NM start another saved network at once; our activation of the
     paused one interrupted that association 1-2 ms later and wpa_supplicant often rescanned (2.3 s). Fixed: activate
     first, auto-connect on afterwards (a7e9766). Since then no competing activation in the journal.
  2. One router DHCP answer 2.0 s late (ip-config 94.85 → lease 96.86), not ours.
  Also: the device was `activated` 0.43-0.55 s after the call while the helper logged 503-753 ms (250 ms polling);
  now 50 ms (883899f). Whether the same-access-point hint itself helps was not isolated; it stays (it costs nothing
  when the access point object is gone: the call fails and the plain activation follows).
- Crash path (run 4): `kill -9` of the helper during `receive`; `airdrop recover` restored everything (Wi-Fi
  connected, no awdl*/OWL left). The CLI printed "Wi-Fi restored" although it was not: fixed (b60c45a), the CLI now
  sees the missing `down` and runs `recover` itself; re-run: "The AWDL helper stopped without cleaning up; undoing
  its session…", Wi-Fi back.

### E-VHT80: receive at 80 MHz (2026-11-14) — **confirmed: 14.8 → 21.0 MB/s; borrow default since API 10**
- Why now: the "VHT80 is not worth building" conclusion (E2.10 era, line ~460) rested on the duty-cycle reading
  that was corrected on 2026-11-11. P2.5 measured **~27 MB/s while the iPhone is present**, in A-MPDUs at HT MCS
  15/40 (300 Mbit/s), so the rate inside a visit matters again.
- Evidence (`20261113-222500-rust-send/air.pcap`, `awdl-decode.py`): the iPhone runs 149 in operating class
  **0x80 = 128 (80 MHz)** (`95 80` in its channel sequence) and advertises VHT as a plain 802.11 VHT Capabilities
  element inside TLV **0x11**: `bf 0c 76f0b113 faff0000 faff0020` (2 streams, MCS 0-9). Our OWL sent HT only.
- Machine: MT7921 VHT RX 2 streams MCS 0-9, short GI 80 MHz (`iw phy phy1 info`). The regulatory domain here allows **5725-5875 MHz at
  80 MHz, 14 dBm, no DFS / No-IR** (decoded from `/usr/lib/firmware/regulatory.db`).
- Built: OWL `-V` adds TLV 0x11 with our element (RX LDPC, short GI 80, A-MPDU exponent 3, no beamforming,
  0xfffa maps); checked off the radio: `11 0e 00 bf 0c 30 00 80 01 fa ff 00 00 fa ff 00 00`. Helper `--vht80`
  (hidden) starts the GO at VHT80 (centre 5775 MHz) and passes `-V`.
- Run (after `scripts/build-owl.sh --install` and installing the API 9 helper; same video, A then B):
  ```sh
  NAME=rx-base DUR=120 AIRDROP=$PWD/target/release/airdrop scripts/airdrop-trace.sh --mode borrow
  NAME=rx-vht80 DUR=120 AIRDROP=$PWD/target/release/airdrop AIRDROP_HELPER_ARGS=--vht80 scripts/airdrop-trace.sh --mode borrow
  python3 -I scripts/analyze-trace.py probe/sessions/*-rx-base probe/sessions/*-rx-vht80
  ```
- Prediction: `sends to us at` shows a VHT rate (80 MHz), more than 15 MB/s. Refuted if: the GO does not start at
  80 MHz (helper error), the iPhone stays on HT rates, MAC loss rises, or the speed does not change. If it wins, make
  it the borrow default on 149 (with `--narrow` still the escape).
- **Result (2026-11-14 22:58 / 23:00, same 25 items = 150.9 MB from the iPhone 17: photos, HEIC/JPEG, one video):**

  | | `rx-base` (HT40) | `rx-vht80` |
  |---|---|---|
  | iPhone sends to us at | HT MCS15/40 91 %, MCS14/40 8 % | **VHT MCS9 ×2 /80 87 %**, MCS7 8 %, MCS5 2 % |
  | transfer | 10.2 s = **14.8 MB/s** | 7.2 s = **21.0 MB/s** (+42 %) |
  | air time in bursts | 8.78 s (25.6 % duty) | 5.79 s (20.0 % duty) |
  | MAC loss of our frames | 0.0 % of 31,936 | 0.0 % of 30,918 |

  Both predictions held (VHT rates; > 15 MB/s). The iPhone still decides when it visits (duty went *down*: it needs
  less air for the same bytes), so the gain is all rate. `analyze-trace.py` learned the radiotap VHT field for this.
  One run each; the mechanism is visible frame by frame, so it was promoted: **API 10 makes 80 MHz + `-V` the
  borrow default on 149-161** (0c8feb6). The GO falls back to 40 MHz if 80 MHz is refused, and OWL gets `-V` only
  when nl80211 reports the GO's channel as 80 MHz wide. `--ht40` (hidden) = the old behaviour; `--vht80` also turns
  it on for 36-48 (allowed here, not run).

### E-GRO: fewer TCP ACKs on receive (2026-11-14) — **refuted by analysis; not built**
- The idea (line ~1044): our pure ACKs take air from the iPhone's bursts; at one ACK per two segments, thinning
  them would give it ~25 % more of each burst.
- **We already send one frame per ~7 of its segments.** `20261112-005449-rx-guard2` (12.2 MB/s): 6,154 unicast
  frames from us against 42,979 full data frames from the iPhone. OWL holds one unicast frame at a time until the
  peer's slot, so the kernel's ACKs queue up and are cumulative by then. The 25 % estimate assumed 1:2.
- **The planned mechanism does not exist for a tap.** OWL reads the monitor one frame per event
  (`pcap_dispatch(..., 1, ...)`) and hands each to `awdl0` with `write()`. `MSG_MORE` batching (what lets GRO merge
  under `IFF_NAPI`) is only reachable through the kernel-side `sendmsg` path, not a userspace tap write. The
  remaining variant (`IFF_NAPI` + `gro_flush_timeout`) is speculative for at most a few percent; not worth the risk
  to a path that works at 12 MB/s.

### E-TXAGG: let the firmware aggregate what we send (2026-11-15) — **built, not yet on the radio [LIVE]**
- Why: send is 5.5-7.0 MB/s against 21 MB/s on receive, and the gap is all per-frame overhead. Every frame we send
  is injected alone (0.25-0.28 ms each, payload ~40 µs of it; "A-MSDU on send" above). The iPhone sends to us in
  A-MPDUs. "Our own injected frames cannot be aggregated (A-MPDU is built by the firmware only for its own station
  entries)" is true, and it is also the way in: give the firmware a station entry for the iPhone.
- Evidence it can work (`20261114-230004-rx-vht80/air.pcap`, read with a throwaway pcap scan):
  - The iPhone already runs Block Ack with us on AWDL: 3 ADDBA requests (TID 0, window 64), each with
    **addr3 = the AWDL BSSID `00:25:00:ff:94:73`**, answered by OWL `-A`.
  - None of its 110,259 data frames has the power-management bit set, so an access point's firmware will not start
    buffering for it.
  - mac80211 looks up the station of an injected frame by addr1 on the interface whose address is addr2
    (`ieee80211_tx_prepare`), numbers QoS frames per TID and queues them on the station's queue; mt76 starts a TX
    Block Ack session for any station with HT (`mt76_connac2_tx_check_aggr`), and a frame without a fixed rate carries
    neither `FIX_RATE` nor `BA_DISABLE` in its descriptor (`mt76_connac2_mac_write_txwi`).
  - One thing is in the way: mac80211 sends ADDBA with addr3 = our address and drops the answer when addr3 is not
    ours (`ieee80211_accept_frame`, AP case). AWDL uses its own BSSID.
- Built (all off unless asked for):
  - **Driver** `vendor/mt76-driver/mt7921e-ratefix-agg-<kernel>.ko` (patch
    `patch/mt7921-family-driver-ht-agg.diff` = the HT build plus this): module parameter
    `/sys/module/mt7921e/parameters/awdl_ba` (default off). While on: Block Ack action frames sent on an AP interface
    to a station get addr3 = the AWDL BSSID, received ones with that BSSID get addr3 = addr1 (both in mt7921e only:
    `mt7921e_tx_prepare_skb`, and a wrapper around `mt7921_queue_rx_skb`); injected QoS data without a rate to a
    station no longer asks for a TX status per frame. `mt7921-common` stays stock. `scripts/mt7921-ratefix.sh load-agg`.
    The refuted monitor-channel hunk is out of the source tree (still in `patch/mt7921-monitor-channel.diff`).
  - **OWL `-G <ap-iface>`**: reads each peer's HT TLV (0x07) and the VHT element in TLV 0x11; on the first unicast
    frame to a peer it registers the peer with `NL80211_CMD_NEW_STATION` (associated, authorized, WME, those
    capabilities; at most 8 at a time), then sends it QoS Data on TID 0 with an empty radiotap header. A refusal is
    logged once and the peer keeps getting single frames at `-r`. ADDBA requests from a registered peer are left to
    mac80211. Stations go with the peer (and with the GO at teardown).
  - **Helper `--tx-agg`** (hidden; borrow mode): switches `awdl_ba` on (old value in the state file's `pm` map, so
    teardown and `recover` put it back), passes `-G awdlgo`, and at teardown logs `iw dev awdlgo station dump` and
    mac80211's `agg_status` per station. The loaded build is recognised by the parameter, and counts as an HT build.
  - Checked off the radio: the QoS frame bytes and both TLV parsers (with the iPhone's own bytes; a short TLV is
    skipped, not an error); QA 18/18; clippy clean. **Not checked:** anything that needs root or the radio.
- Run (iPhone on Everyone, joined to Wi-Fi, same file both times; A then B):
  ```sh
  cd $PWD
  scripts/build-owl.sh --install          # OWL with -G
  scripts/mt7921-ratefix.sh load-agg      # Wi-Fi drops ~3 s; `status` then shows awdl_ba=N
  H=$PWD/target/release
  NAME=send-base DUR=120 AIRDROP=$H/airdrop AIRDROP_HELPER=$H/airdrop-helper scripts/airdrop-trace.sh send --mode borrow -r iphone FILE
  NAME=send-agg  DUR=120 AIRDROP=$H/airdrop AIRDROP_HELPER=$H/airdrop-helper AIRDROP_HELPER_ARGS=--tx-agg scripts/airdrop-trace.sh send --mode borrow -r iphone FILE
  python3 -I scripts/analyze-trace.py probe/sessions/*-send-base probe/sessions/*-send-agg
  grep -E 'station' /run/airdrop-owl.log; grep -E 'station|awdl_ba' /run/airdrop-helper.log | tail -30
  ```
- Predictions, in the order they can fail (each has its own line of evidence):
  1. `owl.log`: `station: registered 02:00:00:00:00:0a (HT caps 0x886f, 2 streams, VHT)`. Refuted by `could not register`
     (the error names why: the kernel or the driver refused the entry).
  2. The send still completes (frames to a station leave the radio and the iPhone accepts QoS Data from us).
     Refuted if B never connects or stalls while A works; then `--tx-agg` is off again by not passing it.
  3. `agg_status` in the helper log shows TID 0 with TX = 1 (the iPhone answered mac80211's ADDBA request), and
     `air.pcap` has a Block Ack action frame from the iPhone with action code 1. Refuted by TX = 0: the iPhone
     ignores or refuses our request (then look at whether it answered at all: action code 1 with a status).
  4. Speed: more than 12 MB/s (double), `tx bitrate` in the station dump at a VHT rate. Refuted if the speed stays
     at 5-7 MB/s with a session up: then the firmware does not aggregate these frames, or OWL feeds one frame per
     event too slowly (compare `analyze-trace.py` frame gap in A and B).
  5. No harm: Wi-Fi back as usual; `cat /sys/module/mt7921e/parameters/awdl_ba` prints `N` afterwards.
- Known unknowns: the firmware's rate control sees every frame that is in flight when the iPhone leaves the channel
  as a loss (OWL's ladder ignored those with a 2 s window); an iPhone that is absent for long may get its rate
  pushed down. If 3 holds and 4 does not, that is the first place to look (`tx retries` / `tx failed`).
- If it wins: receive may gain too (mac80211 then answers the iPhone's ADDBA itself and the driver reorders), and
  `--tx-agg` becomes the borrow default when the build with `awdl_ba` is loaded (helper API 11).

### E-TXAGG live result (2026-11-16 09:16-09:20, campus, borrow on 149 at 80 MHz, 417 MB video) — **refuted as built; one probe left**
| run | session | result |
|---|---|---|
| A baseline (agg driver build loaded, `awdl_ba` off) | `20261116-091621-send-base` | 416.8 MB in 77.0 s = **5.4 MB/s**, 66.5 % duty, frame gap 0.28 ms, MAC loss 0.1 % |
| B `--tx-agg`, first try | `20261116-091804-send-agg` | never connected (`/Discover` refused; 30 frames to the station, 56 retries); stopped by hand |
| B `--tx-agg`, second try | `20261116-091840-send-agg` | ~93 MB in 40 s (**~2.4 MB/s**), then no answer from the iPhone; stopped by hand |

Against the predictions:
1. **Held.** `station: registered 02:00:00:00:00:0a (HT caps 0x886f, 2 streams, VHT)`.
2. **Half.** QoS Data to the station leaves the radio and the iPhone's TCP takes it (67,830 frames), but slower than
   single injected frames, and the transfer died (below).
3. **Refuted.** `agg_status` TID 0: TX = 0, `next dialog_token 0xa`. mac80211 asked about five times (its DELBA
   after each 1 s timeout is in `air.pcap` at 09:18:45, 09:19:02, :34, :51, 09:20:08, addr3 rewritten as intended);
   no ADDBA response from the iPhone is in the capture. Whether the requests themselves were sent cannot be seen
   (our own frames only appear when the driver asked for a TX status). Every attempt also stops the station's queue
   for its 1 s wait (09:19:01-02: half the traffic).
4. **Refuted** (follows from 3). The station dump's `tx bitrate 6.0 MBit/s` is not the data rate (1,800 frames/s of
   1.4 KB went out); the real rate is unknown.
5. **Held.** `awdl_ba` back to 0, Wi-Fi restored, both times.

What else the capture shows:
- **Receive side changed too, unintended.** With the station present mac80211 answered the iPhone's ADDBA (status 0,
  parameters 0x1003 = A-MSDU allowed; OWL answers 0x1002). The iPhone asked **7 times in 7 s** (09:18:45-52) where
  it asks once in a baseline run, so it did not keep the sessions it got. This is a confound of the build, not a
  finding about the idea.
- **The stall (09:19:28.99):** the iPhone's ACKs stop between two frames while it is still on the channel; no RTS
  and no data from it afterwards; our TCP retransmits (0.26, 0.56, 1.0, 2.0 s apart) are acknowledged by its radio
  and ignored above it. At 09:19:30 it went back to its idle sequence (5/16 slots). No power-save bit in any frame.
  Cause not found. Ruled out: our radio leaving the channel (0 off-channel frames), our side not sending.
- The baseline is unharmed by the new driver build with the parameter off (5.4 MB/s, same frame gap as 11-13).

**Conclusion:** as built, `--tx-agg` is worse than the default and is not to be used. The idea stands or falls on
one question the run could not answer: does an iPhone accept a Block Ack request from an AWDL peer at all?

**Probe (built 2026-11-16, OWL `-B`, helper `--ba-probe`, hidden):** on a normal send (default path, no station,
`awdl_ba` off) OWL injects up to three ADDBA requests shaped like the iPhone's own (TID 0, window 64, parameters
0x1003, BSSID AWDL), 2 s apart, each while the iPhone is in our slot, and logs any ADDBA response or DELBA from it.
Our data frames stay non-QoS, so a session the iPhone grants changes nothing for the transfer.
```sh
scripts/build-owl.sh --install
H=$PWD/target/release
NAME=ba-probe DUR=120 AIRDROP=$H/airdrop AIRDROP_HELPER=$H/airdrop-helper AIRDROP_HELPER_ARGS=--ba-probe scripts/airdrop-trace.sh send --mode borrow -r iphone ~/Downloads/AirDrop/IMG_0688.MOV
grep 'Block Ack' /run/airdrop-owl.log
```
- `answered request N: status 0` → the iPhone does grant it; the fault is in how mac80211's request reached it, and
  E-TXAGG is worth a second build (receive side left to OWL, requests sent by OWL).
- `status` non-zero, or three `sent request` lines and no answer → the iPhone does not aggregate for us; **E-TXAGG
  is closed** and send stays at the single-frame ceiling.

### E-TXAGG probe result (2026-11-16 09:51, `20261116-095118-ba-probe`) — **the iPhone grants it**
- Normal send with `--ba-probe`: 132.6 MB in 25.8 s (5.1 MB/s), unaffected. Request 64 (sent 7 s before the
  transfer) got no answer; requests 65 and 66, sent during the transfer, were each answered within the second:
  **status 0, TID 0, window 64, A-MSDU 1.** So an iPhone accepts a Block Ack session from an AWDL peer when the
  request is in AWDL's frame format and arrives while data flows. What failed on 11-16 morning was mac80211's
  request, not the idea.

### E-TXAGG second build (2026-11-16) — **built, not yet on the radio [LIVE]**
- Changes against the first build:
  - **OWL negotiates.** After registering the peer (`-G`), OWL sends the ADDBA request itself (as in the probe),
    holds data to that peer for the answer (50 ms at most; three tries, 2 s apart) and numbers the QoS Data frames
    itself (radiotap TX flag 0x0010, own 12-bit counter per peer; the request's start number is the next frame's).
  - **Driver only listens** (`awdl_ba`, same parameter, new build `8D56E4E8…`): an ADDBA response with status 0
    from a station, BSSID AWDL, switches the firmware's originator Block Ack on for that station and TID
    (`mt76_connac_mcu_sta_ba`, from a work item under the wiphy lock); a DELBA from the recipient switches it off.
    Each change is one kernel log line `awdl_ba: <mac> tid 0: aggregation on (window 64)`. No frame is rewritten
    any more, and mac80211's own session start is kept off these stations (the 1 s queue stops of the first build).
  - **Receive side as in a normal run:** OWL answers the iPhone's ADDBA requests again; mac80211 never sees them.
- Checked off the radio: frame bytes (QoS Data with our number, the request); QA 18/18. Not checked: the rest.
- Run (after `scripts/build-owl.sh --install` and `scripts/mt7921-ratefix.sh load-agg`):
  ```sh
  H=$PWD/target/release; F=$PWD/probe/testfiles/big_buck_bunny_720p_h264.mov
  NAME=send-agg2 DUR=120 AIRDROP=$H/airdrop AIRDROP_HELPER=$H/airdrop-helper AIRDROP_HELPER_ARGS=--tx-agg scripts/airdrop-trace.sh send --mode borrow -r iphone "$F"
  grep -E 'station|Block Ack' /run/airdrop-owl.log; journalctl -k --since -5min | grep awdl_ba
  python3 -I scripts/analyze-trace.py probe/sessions/*-send-agg2
  ```
- Predictions: (1) `Block Ack: asked ...` then `answered request 64: status 0`; (2) kernel line `aggregation on`
  (refuted by `firmware refused` or no line: the driver did not see or apply the answer); (3) more than 12 MB/s
  (5.4 today without); (4) no stall. If (1)-(2) hold and the speed stays at ~2.4 MB/s as in the first build, the
  firmware does not aggregate frames numbered by the host, and that closes it. A stall like 09:19:29 with the
  receive side untouched would point at the station entry itself.

### E-TXAGG second build, live (2026-11-16 10:02, `20261116-100233-send-agg2`, campus, 417 MB) — **confirmed: 5.4 → 10.3 MB/s**
| | baseline 09:16 | `--tx-agg` (second build) |
|---|---|---|
| 416.8 MB | 77.0 s = 5.4 MB/s | **40.6 s = 10.3 MB/s** |
| frames to the iPhone / s | ~3,900 | ~7,600 (`tx unicast` 37-39 k per 5 s) |
| while the iPhone is here (50 ms bins of its ACKs) | — | mostly **14-18 MB/s** |

- Predictions: (1) held (`asked ... request 65` → `answered request 65: status 0, window 64`; request 64, sent
  before any data, got no answer, as in the probe); (2) held (`awdl_ba: 02:00:00:00:00:0a tid 0: aggregation on
  (window 64)`); (3) 10.3 MB/s, short of the 12 predicted, 1.9x the baseline; (4) no stall. Wi-Fi restored,
  `awdl_ba` back to 0. Station: 302,983 frames, 19,430 retries (6.4 %), 36 failed.
- **What bounds it now: the width.** `tx bitrate 173.3 MBit/s VHT-MCS 8 short GI VHT-NSS 2` is the 20 MHz rate, and
  14-18 MB/s while the iPhone is present is what 173 Mbit/s carries. The GO is 80 MHz and the station was registered
  with VHT80 capabilities (the iPhone sends to us at VHT MCS 8 x2 / 80). Why the firmware stays at 20 MHz is open.
- OWL logged 110 `unable to inject` (socket full for 40 ms) in 303 k frames.
- The stall of the first build did not recur with the receive side left to OWL (one run).
- **Next, built 2026-11-16:** driver parameter `awdl_bw` (1 = 40 MHz, 2 = 80 MHz: `FIXED_BW` in the descriptor of
  these frames, the rate stays the firmware's; build `879D8C53…`), helper `--tx-agg-bw 1|2` (hidden, restored like
  `awdl_ba`), and the helper logs mac80211's `ht_capa` / `vht_capa` of the station at teardown.
  ```sh
  scripts/mt7921-ratefix.sh load-agg
  H=$PWD/target/release; F=$PWD/probe/testfiles/big_buck_bunny_720p_h264.mov
  NAME=send-agg-bw80 DUR=120 AIRDROP=$H/airdrop AIRDROP_HELPER=$H/airdrop-helper AIRDROP_HELPER_ARGS="--tx-agg --tx-agg-bw 2" scripts/airdrop-trace.sh send --mode borrow -r iphone "$F"
  grep -E 'station' /run/airdrop-helper.log | tail -12
  ```
  Prediction: `tx bitrate` at an 80 MHz rate (585-866 Mbit/s) and more than 15 MB/s. Refuted if the rate line still
  reads 173.3 (the firmware ignores a fixed width without a fixed rate), or if retries and `tx failed` jump and the
  speed drops (80 MHz frames not received: try `--tx-agg-bw 1`).

### E-TXAGG `--tx-agg-bw 2` run (2026-11-16 10:09, `20261116-100935-send-agg-bw80`) — **no result on width; a fault in OWL's asking, fixed**
- The transfer ran unaggregated at ~3 MB/s (2,200 frames/s) until the trace's 120 s limit cut it at ~317 MB. All
  three ADDBA requests went out at 10:09:38-43, **before the iPhone had accepted the transfer** (data started at
  10:09:58), none was answered, and three was the limit. In the 10:02 run the second request happened to fall into
  the transfer. With the probe run that makes 4 of 4 requests before data unanswered, 3 of 3 during data granted.
- Also seen: QoS frames to the station **without** a session are slower than plain injected frames (3 MB/s here,
  2.4 MB/s in the first build, 5.4 MB/s plain). mac80211 has the station at 80 MHz (`vht_capa: ... 80Mhz;
  SHORT-GI-80`), and `tx bitrate` still read 173.3 Mbit/s (20 MHz) with `awdl_bw` = 2 on every frame: a hint that
  the firmware ignores a fixed width without a fixed rate, but not a test (no aggregation in this run).
- **Fixed in OWL:** a request goes out only ahead of a full-size frame (1000 B and more: a transfer is running),
  every 2 s until granted (60 tries); until the peer agrees it gets plain frames at OWL's own rate, exactly as
  without `-G`, so a peer that never agrees costs nothing. Log: `Block Ack: <mac> agreed; its frames are aggregated
  from here on`. Re-run the same command after `scripts/build-owl.sh --install`.

### E-TXAGG re-run with the asking fix (2026-11-16 10:40, `20261116-104023-send-agg-bw80`) — **asking fixed; `awdl_bw` refuted**
- 416.8 MB in 42.5 s = **9.8 MB/s** (10.3 at 10:02). One request, sent ahead of the first full-size frame at
  10:40:53, granted at once: `agreed; its frames are aggregated from here on`. The fix holds.
- **`awdl_bw` = 2 does nothing:** `tx bitrate 173.3 MBit/s VHT-MCS 8 short GI VHT-NSS 2` (20 MHz) again, speed as
  without it. The firmware ignores `FIXED_BW` on a frame whose rate it picks. The parameter stays in the driver
  (default 0) but is not a lever. 25,208 retries in 303 k frames (8.3 %), 88 failed.
- mac80211 has the station at 80 MHz (`vht_capa ... 80Mhz; SHORT-GI-80`), so the firmware decides on 20 MHz itself.
- **Next guess, built:** our GO's beacon carries no HT or VHT elements at all (header, empty SSID, rates), so to the
  firmware the network it sends in may look like a plain 20 MHz one. With `--tx-agg` the helper now starts the GO
  with a beacon tail: HT Capabilities, HT Operation (secondary channel, any width), and at 80 MHz VHT Capabilities
  and VHT Operation (80 MHz, centre 155), claiming what OWL claims in AWDL. Without `--tx-agg` the beacon is as
  before (the receive path that does 21 MB/s is untouched).
  ```sh
  H=$PWD/target/release; F=$PWD/probe/testfiles/big_buck_bunny_720p_h264.mov
  NAME=send-agg-bcn DUR=120 AIRDROP=$H/airdrop AIRDROP_HELPER=$H/airdrop-helper AIRDROP_HELPER_ARGS=--tx-agg scripts/airdrop-trace.sh send --mode borrow -r iphone "$F"
  grep -E 'station: tx' /run/airdrop-helper.log | tail -4
  ```
  Prediction: `tx bitrate` at a 40 or 80 MHz rate and more than 15 MB/s. Refuted by 173.3 again (the beacon is not
  what the firmware goes by; then the remaining way is a fixed rate and width per frame, which gives up the
  firmware's rate control). A GO that does not start (`start access point` error) means the kernel rejects the
  tail: the run then fails before Wi-Fi is paused for long, and is undone as usual.

### E-TXAGG with a full GO beacon (2026-11-16 10:47, `20261116-104701-send-agg-bcn`) — **width confirmed: 20-28 MB/s; upload cut by the receiver at 349 of 417 MB**
- **The beacon was it.** `tx bitrate 780.0 MBit/s VHT-MCS 9 80MHz VHT-NSS 2` (173.3 at 20 MHz in every run before).
  With HT/VHT Capabilities and Operation in the GO's beacon the firmware sends to the station at 80 MHz.
- Speed from the receiver's ACKs, per second: 16-20 MB/s in the first seconds, then **21-28 MB/s** for 11 s
  (`tx unicast` 85-91 k frames per 5 s, against 37-39 k at 20 MHz and ~19 k without aggregation). 253,362 frames,
  23,567 retries (9.3 %), 37 failed; 34 `unable to inject`.
- **Not a completed transfer.** After 19.5 s and 349.0 MB the client reported `upload interrupted: unexpected EOF`:
  the receiver closed the connection while data was flowing at full rate. Cause unknown from our side.
- **The receiver was not the usual phone.** `-r iphone` matched a device named "iPhone" (`02:00:00:00:00:0b`, HT caps
  0x006f, VHT 0x03800032), not "TestPhone" (`02:00:00:00:00:0a`). Somebody accepted the file on it. So this is also the
  first aggregation run against a second device: it granted the ADDBA request at once as well. Use `-r testphone`.
- Open: one complete transfer to TestPhone with the full beacon; whether the EOF repeats (then it is ours); receive
  with the full beacon (unchanged so far: the tail is only sent with `--tx-agg`).

### E-TXAGG complete run to TestPhone (2026-11-16 11:00, `20261116-110005-send-agg-bcn2`) — **confirmed: 5.4 → 30.6 MB/s; default since API 11**
| 416.8 MB to the iPhone 17 | baseline 09:16 | aggregation, 20 MHz (10:02) | aggregation + full beacon (11:00) |
|---|---|---|---|
| time, speed | 77.0 s, 5.4 MB/s | 40.6 s, 10.3 MB/s | **13.6 s, 30.6 MB/s** |
| rate the firmware reports | (OWL: HT MCS 15/40) | 173.3 Mbit/s VHT MCS 8, 20 MHz | **866.7 Mbit/s VHT MCS 9, 80 MHz, short GI** |
| retries / failed of ~303 k frames | MAC loss 0.1 % | 19,430 / 36 | 9,824 (3.2 %) / 44 |

- Completed (`Sent 416.8 MB to TestPhone in 13.6s`). The EOF of the 10:47 run did not repeat on the user's phone, so
  that one was the other device's end (one run each).
- Wi-Fi reconnect took 34.6 s: NetworkManager's journal shows the association done in 0.3 s and then **no DHCP
  lease for 32 s** on the campus network (11:00:29 → 11:01:01, then a re-association). Not ours; the client
  printed "helper did not stop in time" because of it.
- **Made the default (HELPER_API 11):** the client passes `--tx-agg` for `airdrop send` in borrow mode; the helper
  uses it when the loaded mt7921e has `awdl_ba` and the installed OWL has `-G`, and otherwise sends single frames
  as before (a log line, no warning). The full beacon goes with it, so `receive`, `listen` and `find` sessions are
  exactly as before. `scripts/mt7921-ratefix.sh load` now loads the aggregating build when one exists for the
  kernel (`AGG=0` for the HT build). `airdrop doctor` names the three driver states.
- Not yet run: the default path end to end (plain `airdrop send` with the installed helper); a second file size; a
  send from another room (the firmware's rate control stepping down); receive with a station entry (not planned).
- Leftovers in the tree, harmless: driver parameter `awdl_bw` and helper `--tx-agg-bw` (refuted), OWL `-B` /
  helper `--ba-probe` (the probe).

### E-RTW89 Phase 0 (2026-11-22, second laptop: RTL8852BE `rtw89_8852be`, stock driver, kernel 7.3.5-arch1-1, borrow on 149 at 80 MHz) — **confirmed: 0.2 → 5.6 MB/s with a station entry; default for rtw89**
- Question: why does the same code send at 0.15-0.25 MB/s on rtw89 (MT7921 single frames: 5.4 MB/s)?
- Earlier the same day (`20261122-104635-rtw-send`): the chip sends exactly 17 frames,
  the iPhone ACKs within ~2 ms, then ~140 ms of silence; frames wait a median 424 ms between mac80211's monitor
  xmit and their TX status; mac80211 queues empty. Refuted then: rate, BT coex, scans, power saving, AQL/airtime,
  dynamic EDCCA/DIG.
- Hypothesis: injected frames without a station go out under the access point's own mac_id
  (`rtw89_core_tx_get_mac_id`), whose MAC queue the firmware serves in bursts. Prediction: with the iPhone as a
  station of the GO the bursts disappear and the rate approaches 5 MB/s.
- Setup: `scripts/rtw89-phase0.sh DEVICE FILE` (417 MB video, P-RECV, 40 s runs), A = as now, B = helper `--tx-sta`
  (OWL `-g awdlgo`: station entry, no Block Ack request, OWL's radiotap rate kept), C = `--no-ampdu` (did not run:
  the iPhone did not engage).
- TXBD indexes of ACH0 (debugfs `read_reg 1058 10`, 50 ms): host == hw in every sample of A (`00f900f9`): the chip
  has fetched the frames; they wait inside the MAC.

| 40 s send | A as now (`112732-p0-A`) | B station entry (`112824-p0-B`) |
|---|---|---|
| speed | 202 KB/s | **5.6 MB/s** (170.5 MB in ~30 s) |
| our frames ACKed on air | 4104 (bursts of 17, gaps 110-160 TU) | 124,006; 6 gaps >= 100 ms |
| 16 TU slots used | 13 % | 83 % |
| iPhone's frames to us retried | 61 % | 10 % |
| rate | MCS 15/40 | MCS 15/40 throughout, 0 lost; station dump 0 retries / 15 failed of 124 k |

- Conclusion: confirmed. mac80211 attaches the station to injected unicast (tx.c `sta_info_get(sdata, addr1)`) and
  rtw89 then uses the iPhone's own mac_id and address entry. mac80211 cannot start its own BA session here (no
  software rate control on rtw89). Made the default: `crates/airdrop-helper/src/driver.rs`
  (`Family::needs_station`) turns the client's `--tx-agg` into OWL `-g awdlgo` on rtw89.
- Follow-up runs (11:39-11:45, helper with the rtw89 default installed):
  - Plain `airdrop send -r testphone` (no flags): **416.8 MB in 75.2 s, 5.5 MB/s**. The default works end to end.
  - C (`--no-ampdu` on top of the station default, `114036-p0-C`): 4.9 MB/s. Refusing the iPhone's Block Ack costs
    a little; keep `-A`.
  - Receive, the iPhone sending the same 417 MB video: plain `airdrop receive` (no station entry) 563 KB/s (stopped at
    3.5 %); `AIRDROP_HELPER_ARGS=--tx-sta airdrop receive`: **416.8 MB in 20.3 s, 20.5 MB/s** (MT7921: 21.0).
    The chip's handling of the iPhone's A-MPDUs needs the station entry too; no driver change (planned F2) needed.
- Made the default for **every borrow session on rtw89** (send, receive, listen): `start_owl` uses `-g awdlgo` when
  `Family::needs_station()` and no aggregating build is loaded.
- Left: aggregated sending (5.5 → ~30 MB/s on MT7921) needs a patched rtw89 (plan F1).

### E-RTW89 F1 (2026-11-22 11:57, RTL8852BE, our rtw89 build) — **confirmed: send 5.5 → 40.5 MB/s, receive 20.5 → 25.2 MB/s**
- Question: does the MT7921 `awdl_ba` recipe carry over to rtw89?
- Build: `driver/rtw89-awdl.diff` (core.c/core.h, ~160 lines) on v7.3.5, `scripts/rtw89-build.sh`, all five 8852BE
  modules swapped with `scripts/rtw89-load.sh load`. The unpatched build of the same tree is byte-identical to
  Arch's modules (symbols and `.text`); the patched one changes `struct rtw89_dev`, so the stack is only swapped
  together.
- Patch, behind `rtw89_core` parameter `awdl_ba` (helper: on for every rtw89 borrow session, restored after):
  an AWDL-format ADDBA response (status 0) from a station switches firmware aggregation on for it (as
  `IEEE80211_AMPDU_TX_OPERATIONAL`); DELBA from the recipient off; injected frames never start mac80211's own
  session (Phase 0 B had 6 unanswered mac80211 ADDBA requests, each stopping the queue up to 1 s); injected
  frames without a radiotap rate keep firmware rate control. Helper: OWL `-G`, HT/VHT in the GO beacon.
- Prediction: `awdl_ba: ... aggregation on` in the kernel log and >= 20 MB/s.
- Result:
  - `airdrop send -r testphone` 417 MB video: **416.8 MB in 10.3 s, 40.5 MB/s** (MT7921 best: 30.6). OWL asked once
    ("request 64, starting at 0"), the iPhone agreed at once; kernel: `awdl_ba: 02:00:00:00:00:0a tid 0: aggregation
    on (window 64)`. Station dump at teardown: 302,967 tx packets, **0 retries, 0 failed**, tx bitrate
    **866.7 Mbit/s VHT-MCS 9 80 MHz short GI NSS 2**.
  - `airdrop receive`, the same video from the iPhone: **416.8 MB in 16.5 s, 25.2 MB/s** (stock + station: 20.5;
    MT7921: 21.0). The receive session also got "aggregation on" (OWL asks before bulk frames it sends).
  - `awdl_ba` back to N after each session (`restore awdl_ba` in the helper log). No warnings; the only other
    kernel line is rtw89's `timed out to flush queues` when the GO stops, which the stock driver printed in
    earlier sessions too (frames left for a peer that went off channel; teardown 1.4 s instead of ~1.0).
- Conclusion: confirmed. rtw89 + this build is the fastest card so far in both directions.
- Open: soak (several sends, a multi-GB file, listen, suspend/resume, a day of normal Wi-Fi with the build loaded);
  a send from another room (firmware rate control stepping down).
