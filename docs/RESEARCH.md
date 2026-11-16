# AirDrop on Linux: how it works, what others found, what it means here

Research notes behind this client's design (November 2026). Sources are listed at the
end. "Measured" marks something seen on the wire on this machine (MediaTek MT7921,
`14c3:7961`, router on channel 36, iPhone 17 on iOS 27.2).

## 1. The stack, top to bottom

| Layer | Current behaviour (iOS 26–27) |
|---|---|
| Archive | cpio, `newc` (`070701`) or `odc` (`070707`), ends with `TRAILER!!!`. iOS wraps each item in an `NSIRD_AirDrop_xxxxxx/` staging folder and adds `._` AppleDouble sidecars. A root `.` member is common. |
| Container | `application/x-dvzip`: blocks of `[4-byte BE header][payload]`; bit 31 = stored, low 31 bits = payload length, otherwise a zlib stream; 128 KiB of cpio per block; no trailer. gzip'd cpio is the fallback when `SupportsDVZip` is absent. |
| Encoding | Binary plists (`bplist00`) for `/Discover` and `/Ask` bodies. |
| HTTP | HTTP/1.1, all POST, all on **one** TLS connection (an `/Upload` on another connection is refused). Endpoints: `/Discover`, `/Ask`, `/Upload` (plus `/Hello`, `/Exchange`, `/SharedIdentity`, `/Error` seen in recent iOS). iPhones send only `User-Agent: AirDrop/1.0`, `Connection`, `Transfer-Encoding: chunked`. |
| TLS | TLS 1.2/1.3, self-signed certificates, no hostname check, no client certificate. Identity in Contacts-only mode comes from an Apple-signed sender record, not from TLS. |
| Network | IPv6 link-local on `awdl0`; receiver advertises `_airdrop._tcp` with a TXT `flags` bitmask (Apple sets ~`0x37FF`; `0x0A` = DVZip + MixedTypes is enough to be listed). |
| Link | **AWDL** (Apple Wireless Direct Link), woken by a **BLE** advertisement. |

### The transfer, in order
1. **BLE wake.** The sender broadcasts Apple manufacturer data, Continuity type `0x05`
   (AirDrop): 8 bytes, a version byte, four 2-byte truncated SHA-256 hashes of contact
   identifiers, `0x00`. It only means "an AirDrop sender is near, switch AWDL on". It
   names no channel or address. Measured: iOS 27 itself advertises version `0x03`;
   senders using `0x01` with zero hashes are accepted in Everyone mode.
2. **AWDL sync.** Nodes exchange action frames (`7f 00 17 f2 08 …`), elect a sync
   master, and publish a 16-slot channel sequence. Data flows only in slots where both
   are on the same channel. Measured: the iPhone's sequence is mostly its router's
   channel (`36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36`); slot 8 is always 6.
3. **Discovery.** A *sender* browses `_airdrop._tcp.local.` over mDNS on `awdl0`; a
   receiver answers with PTR/SRV/TXT/AAAA. iOS browses AirDrop on `awdl0` only — never on
   ordinary Wi-Fi (macOS has `BrowseAllInterfaces`, iOS has no equivalent).
4. **`/Discover`.** Receiver replies with `ReceiverComputerName`, `ReceiverModelName`,
   `ReceiverMediaCapabilities` (and a `StatusCode`: 100 Everyone, 200 contact match,
   401 not reachable).
5. **`/Ask`.** Sender's name/model/id, `Files[]` (or `Items[]` for links), a JPEG 2000
   `FileIcon` preview (~50 KB from iOS), and on iOS 26+ `TransferID = {id: UUID}` +
   `TransferType = {files: {}}`. **iOS times the whole /Ask round trip**: a receiver that
   answers after reading the (slow) body, or after the user decides, is reported as
   "declined". Answer 200 first, ask the user afterwards ("early ask").
6. **`/Upload`.** Chunked dvzip body with `TotalBytes`, `TransferID` (must match the
   `/Ask`, or the receiver refuses on the headers), `SenderPseudonym`, `SenderPushToken`.
   **iOS withholds the final empty chunk until the receiver replies**, so a receiver
   must answer when the archive is complete, not when the body ends.

### Modes
- *Everyone for 10 minutes*: works for non-Apple peers in both directions.
- *Contacts only*: needs an Apple-issued identity (three secrets, not one); not
  achievable without an Apple ID.
- *AirDrop codes* (iOS 26.2): a one-time code shown on the receiver, typed by the
  sender, for unknown senders; does not replace Everyone mode.

## 2. Other implementations

| Project | What it shows |
|---|---|
| **OpenDrop / OWL** (TU Darmstadt, 2018–19) | The original reverse engineering of AWDL and AirDrop. OWL needs a card with *active* monitor mode and injection. |
| **airdrop-mt7921** | AirDrop on the same chip as this laptop. Detailed FINDINGS: fresh monitor vifs (in-place type switch never retunes), plain + `flags active` vif pair, mt76 runtime-pm/deep-sleep must be 0, iOS rejects a constant ("pin") channel sequence, dvzip, the withheld final chunk, `TransferID`. Receiving works ~40–67 kB/s; with Wi-Fi kept associated a transfer was never confirmed. Their §46: a **P2P-GO** vif gives the radio a second channel context while Wi-Fi stays associated. |
| **omdrop-owl / Owly** (Rust) | Card matrix: MT7925 works with Wi-Fi up; **MT7921 shared fails** on the stock driver; driver-free workarounds tried and failed (group-addressed frames are dropped by iOS, radiotap rates ignored). |
| **shaecodes-wilde/mt76** | Root cause on MT7921: injected unicast data goes out at 1 Mbit/s CCK (rate-inject flag erased), which cannot be sent on 5 GHz. Patched `mt7921e` fixes it. |
| **Windfall** (Windows, Kali + OWL bridge) | iOS 26.6/27.0 accept a non-Apple receiver end to end. Intel AX211 on **channel 6**, no active monitor: 3.6 MB photo intact at ~16 KB/s with one overlapping slot in 16. Early ask fixed every decline. The phone cannot be pulled onto channel 6 by joining it to 2.4 GHz Wi-Fi. |
| **Google Quick Share ↔ AirDrop** (Pixel 10, 2025) | Proves a non-Apple stack can do Everyone-mode AirDrop at product quality; built in Rust, transport not documented. |
| **"Protocol Prying"** (arXiv 2606.26967) | The full current application-layer stack (7 layers, endpoints, flags), plus three pre-auth DoS bugs in Apple's parser stack. |

### What Apple is (and is not) opening
The EU DMA specification (case DMA.100203) requires Apple to make AirDrop interoperable
over BLE / P2P Wi-Fi ("AWDL or Wi-Fi Aware") and to publish a protocol specification. As
of this writing no specification is public; iOS 26 ships a **Wi-Fi Aware** API for apps,
but AirDrop itself still runs over AWDL. Linux Wi-Fi Aware (NAN data path) is still being
merged and is Intel-only; nothing exists for mt76, so it is not a route for this card.

## 3. What this means for this laptop

Measured status with Wi-Fi kept connected (shared mode, channel 36):
- BLE wake works; OWL syncs to the iPhone and keeps it as a peer; the iPhone's mDNS
  queries arrive on `awdl0`; once, the iPhone opened a TCP connection to us.
- What has never worked: **our unicast frames reaching the iPhone** (0 of 56 ACKed).
  That is the MT7921 driver's 1 Mbit/s CCK fallback on 5 GHz, and no userspace change can
  alter the rate the driver puts in the TX descriptor.

The radio constraint therefore decides the design. Options, all keeping Wi-Fi up:

| Option | Driver change | Wi-Fi impact | Expected result |
|---|---|---|---|
| **A. Shared on 36** (current) | no | none | Discovery yes, unicast replies never air → no transfer |
| **B. P2P-GO on channel 6** beside the station on 36; monitor vifs carry the GO's MAC so injection uses its 2.4 GHz channel context | no | ~40 ms extra latency while AirDrop runs (radio time-slices 36/6) | 1 Mbit/s CCK *is* valid on 2.4 GHz → unicast airs; overlap is slot 8 only (~1/16) → Windfall-like ~10–16 KB/s, small photos |
| C. Patched `mt7921e` | yes (temporary module) | none | Unicast airs on 36 at a proper rate; overlap 6–7/16 slots → the best throughput this card can do |
| D. Second Wi-Fi adapter for AWDL | no | none | Depends on the adapter (MT7612U, MT7925 reported working) |

**B** is the only path that satisfies all of: no driver install, Wi-Fi stays connected,
and frames that the stock driver can actually transmit. It is unproven on this card for a
full transfer (airdrop-mt7921 §46 proved sync and an ICMPv6 round trip, not a file).

## 4. Result (2026-11-02 21:28): it works on this laptop
iPhone 17 (iOS 27.2) → this MT7921, **Wi-Fi kept connected on channel 36**: a 6.87 MB
photo in 7.9 s (~870 KB/s), intact. What it took, all three together:
1. **Regulatory domain set to the real country** (`sudo iw reg set <country code>`). After a reboot
   the card sat in the world domain `00`, where every 5 GHz channel is No-IR and mac80211
   silently drops injected frames — no error, no counter. This hid every other fix.
2. **The rate-fixed `mt7921e`** (`scripts/mt7921-ratefix.sh load`), so unicast replies
   leave at the requested OFDM rate instead of 1 Mbit/s CCK.
3. This client: plain + active monitor pair, OWL following the iPhone's sequence with
   radio-aware TX, early ask, dvzip, completion-time reply.
`airdrop doctor` and the helper now check for (1).

## 5. Already applied in this client (from the above)
Early ask; dvzip in both directions; reply when the archive completes; `NSIRD_` /
AppleDouble flattening; `TransferID` + Apple's header set when sending; DVZip flag in
TXT; OWL patched for channel 36, master-sequence following and radio-aware TX gating;
plain + active vif pair; mt76 power saving off; fail-safe teardown and `airdrop recover`.

## 6. Update survey (2026-11-12)
Web survey only, no radio. **[R]** = read at the source by this session, **[A]** = reported by a
search agent from the source, **[S]** = search snippet or press only (treat as a lead).

### Nothing changes the design
- **No one publishes faster AWDL numbers than ours.** Best found: owly #82, Intel AX211, IBSS cell on
  149 with a patched mac80211 answering ADDBA: "43 MB video in ~17 s = ~2.46 MB/s"; ch 6 without
  Block Ack "~0.2 to 0.4 MB/s" [R]. shaecodes mt76 fork: 727 KB upload in 3.69 s on MT7922 [A].
  Ours (11-12): send 5.6 MB/s, receive 12.2 MB/s.
- **No evidence that AirDrop moved to Wi-Fi Aware (NAN)**, with Apple or with Android peers. Google's
  post names no transport; press sources contradict each other [A]. AWDL stays the link.
- **NAN on Linux is still not usable here.** hostap 2.12 (2026-08-07) adds "synchronized NAN; extend
  USD support" [A]; the NAN data path series for mac80211 is still in review and points at an
  iwlwifi branch [S]. Nothing for mt76. No report of Linux ↔ iOS Wi-Fi Aware interop.
- **No upstream fix for the radiotap rate on mt7921e.** Our per-boot rate-fixed module is still
  needed; shaecodes-wilde/mt76 carries the same kind of fix for MT7920/7921/7922 [A].

### New facts worth acting on
1. **owly 0.4.3 (11 Nov)**; the copy we read was 0.4.2. Release notes: "Specify upload, timeout
   and BLE details seen from an iPhone" (#114), client certificate + link share support (#113), a fix
   for multiple photos from Photos (#111) [R]. Its `docs/open-questions.md` overlaps our §12: BLE v1
   vs v3, `/SharedIdentity` and `/Error`, a 401 status on `/Discover`, whether macOS times out `/Ask`
   [A]. → refresh the vendored copy and diff SPEC.md against PROTOCOL.md.
2. **iOS 27.0 vs 26.6 (Windfall notes, their captures):** the JPEG 2000 preview grew from 30–70 KB to
   127–215 KB and `/Ask` took 10–19 s instead of ~8 s [A]. Matches why early-ask matters here; worth
   checking the preview size in our own dumps.
3. **mac80211 drops monitor injection next to another vif on chanctx drivers (mt76 named).**
   "wifi: mac80211: fix monitor mode frame capture for real chanctx drivers", v5 2026-06-03, tx.c
   only, tested on mt7921e; not merged as far as found. Johannes Berg held it because injection with
   the patch crashed some mt76 devices [R/S]. A second attempt ("restore monitor injection when
   coexisting with another VIF", 2026-04-24, Fixes 0a44dfc07074) was dropped [R]. Our injection beside
   the GO works on 7.2.5 today; why it is not hit was **not** checked. First suspect if injection goes
   silent after a kernel bump.
4. **Firmware AWDL on Broadcom Macs** (not our hardware, but an independent view of the protocol):
   a brcmfmac series by Andre Eikmeyer for T2 Macs (Phoronix 2026-11-05; firmware keeps the schedule
   and data path, election/discovery/AirDrop in user space, tool `t2-drop`) [S, article returned 403];
   brentkearney/omdrop-awdl for BCM4378/4387/4388 [R]: master AW channel 6, peer AW channel 44,
   election metric 100, `awdl0` MTU 1484, firmware peer table of 8 entries, "Apple devices change
   their AWDL MAC every few minutes"; andreanicassio/brcmfmac-awdl (discovery only) [R].
5. **owly #82 details** [R]: under regdom `00` 149 is listen-only (same trap as §4 here); an AP/GO
   "cannot answer Block Ack for AWDL" on Intel (own BSSID, expects ToDS) — on our MT7921 the GO does
   answer A-MPDUs (11-11 run), so this is card-specific; a fixed cell on 149 did not pull an iPhone
   there on Intel ("follows us rather than pulling us to 149").
6. **Apple / EU:** DMA page: close-range transfer measures due in iOS 27 and by end of 2026, interim
   solution 1 June 2026; Wi-Fi Aware 5.0 within nine months of the spec [A]. iOS 27 beta notes are
   said to list "Faster AirDrop transfers" and faster recipient discovery [S]. Quick Share ↔ AirDrop
   reached Pixel 9 (MacRumors 2026-02-17), still Everyone-for-10-Minutes only [A].

### Checked and not new
jedbillyb FINDINGS §46 (GO on 149 beside a station) is already in our copy and was refuted for this
card in E0.3b. seemoo owl is archived; opendrop has nothing on iOS 26/27.

### Looked for, not found
Wire details of `/Hello`, `/Exchange`, `/SharedIdentity`, `/Error` on the web (from our own firmware instead:
PROTOCOL §7.2.1, S-26); any iOS 27 change to channel
sequences, TLV 0x21 or BLE advert versions; AWDL deprecation dates; 2026 papers besides arXiv
2606.26967; documentation of what triggers scans on a disconnected NetworkManager device.

## Sources
- 2026-11-12 survey: https://github.com/t4t5/owly/releases , https://github.com/t4t5/owly/issues/82 ,
  https://github.com/brentkearney/omdrop-awdl , https://github.com/andreanicassio/brcmfmac-awdl ,
  https://lkml.iu.edu/2606.0/07175.html ,
  https://patchew.org/linux/20260424120807.25005-1-brite.airgeddon@gmail.com/ ,
  https://phoronix.com/news/Linux-Kernel-Apple-AirDrop-Code ,
  https://lists.infradead.org/pipermail/hostap/2026-August/045441.html ,
  https://digital-markets-act.ec.europa.eu/businesses-portal/interoperability/apple-specification-proceedings-interoperability-connected-devices-and-request-based-process_en ,
  https://www.macrumors.com/2026/02/17/android-iphone-airdrop-pixel-9-support/
- Stute, Kreitschmann, Hollick — *One Billion Apples' Secret Sauce: Recipe for the Apple
  Wireless Direct Link Ad hoc Protocol* (MobiCom 2018), https://arxiv.org/abs/1808.03156
- Stute et al. — *A Billion Open Interfaces for Eve and Mallory* (USENIX Security 2019),
  https://www.usenix.org/conference/usenixsecurity19/presentation/stute
- *Protocol Prying: Systematic Vulnerability Research in the Apple AirDrop and Android
  Quick Share Proximity Transfer Protocols*, https://arxiv.org/html/2606.26967v1
- seemoo-lab/owl and opendrop: https://github.com/seemoo-lab/owl
- jedbillyb/airdrop-mt7921 (docs/FINDINGS.md), jedbillyb/owl, jedbillyb/opendrop
- t4t5/omdrop-owl (issues #4, #14; PR #24): https://github.com/t4t5/omdrop-owl
- shaecodes-wilde/mt76: https://github.com/shaecodes-wilde/mt76
- UvejsGj/Windfall (docs/adr-001, docs/protocol-notes.md): https://github.com/UvejsGj/Windfall
- Google, *Android Quick Share support for AirDrop*:
  https://blog.google/security/android-quick-share-support-for-airdrop-security/
- European Commission, DMA.100203 case summary and proposed measures:
  https://ec.europa.eu/competition/digital_markets_act/cases/20253/DMA_100203_1283.pdf
- MacRumors, *iOS 26.2 AirDrop codes*: https://www.macrumors.com/2025/11/17/ios-26-2-airdrop-codes/
