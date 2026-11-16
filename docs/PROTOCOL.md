# AirDrop protocol, as iOS 27.2 runs it

A layer-by-layer description of AirDrop between an **iPhone 17 (`iPhone18,3`) on iOS 27.2 (24B119)** and a
Linux node (MediaTek MT7921 + OWL), for people building or debugging non-Apple implementations. It extends
owly's independent spec (`docs/SPEC.md` in [owly](https://github.com/t4t5/omdrop-owl); IDs like `ASK-7` refer to it).

Not affiliated with Apple. Interoperability research (EU Directive 2009/24/EC Art. 6). Only names and
strings from Apple binaries are quoted; no Apple code.

## 0. How to read this

Every field, value and behaviour carries one tag:

| Tag | Meaning |
|---|---|
| **[M]** | **measured** here, on the wire or in a dump; the reference is a session in `probe/sessions/` or an experiment ID in `docs/EXPERIMENTS.md` (`R-…`, `E…`) |
| **[S]** | **static**: read from the public iOS 27.2 firmware (symbol, string, disassembly); reference `S-…` |
| **[H]** | **hypothesis**: not proven here. Either inferred, or cited from another project (owly, Windfall, airdrop-mt7921, papers), which counts as a hypothesis for iOS 27.2 until measured. Each names the test that settles it. |

`docs/EXPERIMENTS.md` is the experiment log. Rows tagged [S] come from reading names and strings in the public iOS 27.2 firmware; those notes are not part of this repository. Open items are in §12 with
status **answered**, **open (experiment ID)** or **blocked (reason)**.

Byte order is little-endian unless marked BE. TU = 1024 µs.

---

## 1. Overview

| # | Layer | iOS 27.2 | Tag |
|---|---|---|---|
| 7 | Archive | cpio **odc**: `.`, `./<file>`, `TRAILER!!!` | [M] R-07 |
| 6 | Compression | **DVZip** (`application/x-dvzip`) | [M] R-07 |
| 5 | Encoding | binary plist (`bplist00`) | [M] R-04 |
| 4 | Transport | HTTP/1.1, `POST`, chunked bodies | [M] R-04 |
| 3 | Security | **TLS 1.3**, X25519MLKEM768 offered first; Apple ID client cert on request | [M] R-03, R-08 |
| 2 | Network | IPv6 link-local on `awdl0`, address = EUI-64 of the AWDL MAC | [M] R-03 |
| 1 | Link | **AWDL** (802.11 action + data frames) | [M] R-02 |
| 0 | Wake | **BLE** advert, Apple Continuity type `0x05` | [M] R-01 |

Flow of one transfer, iPhone → receiver, as measured (R-03…R-09):

```
iPhone (sender)                                          receiver
BLE ADV_NONCONN_IND type 0x05 (share sheet open)  ───▶    [M] R-01
AWDL: PSF on ch 6, MIF on its channels; TLV set grows ◀──▶ [M] R-02
  once a peer exists
mDNS QM: PTR? _airdrop._tcp (+2 own services)     ───▶    [M] R-12
                                                  ◀───    PTR/SRV/TXT/AAAA (receiver)
TLS #1 ─ POST /Discover {SenderRecordData, DeviceSupportFlags}            [M] R-04
        ◀── 200 {ReceiverComputerName, ReceiverModelName, …}
        (sometimes a second /Discover on a new connection)                [M] R-05
TLS #2 ─ POST /Ask {TransferID, Files|Items, FileIcon, SenderRecordData,
         SenderIdentityAuthTag, …}                                        [M] R-07
        ◀── 200 (receiver answers first, asks its user while the body streams; [H] Windfall, used here)
       ─ POST /Upload  TransferID: <same>  (DVZip of odc cpio)            [M] R-07
        ◀── 200 (after TRAILER!!!; iOS holds back the final chunk until then) [M]
```

A link share is the same without `/Upload` [M] R-04.

---

## 2. Wake layer: BLE

### 2.1 The AirDrop advertisement (sender)
Measured on every share-sheet session (R-01, R-06, R-09):

| Field | Value | Tag |
|---|---|---|
| PDU type | **`ADV_NONCONN_IND`** (non-connectable, non-scannable) | [M] R-01: 30/30 HCI reports; contact session 105/105 |
| Address | random **resolvable** private address | [M] R-01 |
| Interval | received 69 ms – 1.5 s apart (scanner duty cycle limits this; the source interval is not measured) | [M] contact session |
| Flags AD | `02 01 1a` | [M] |
| Manufacturer AD | `17 ff 4c 00` (Apple), then TLV `05 12` (AirDrop, 18 bytes) | [M] |
| byte 0 | `0x40` | [M] constant in all sessions; meaning **blocked [LIVE] E1.3** (§12 Q-B2) |
| bytes 1–3 | **tag**: constant per BLE address, changes when the address changes (3 addresses → 3 tags) | [M] R-09 |
| bytes 4–7 | `00 00 00 00` | [M] |
| byte 8 | version `0x03` | [M] |
| bytes 9–16 | **4 × 2-byte short hashes**, order **shuffled between advert batches** | [M] R-06, R-09 |
| byte 17 | `0x00` | [M] |

- The hashes are the first 2 bytes of SHA-256 contact hashes: 3 of the 4 equal the prefixes of the
  `ValidatedPhoneHashes[0]` (`aaaa`) and both `ValidatedEmailHashes` (`bbbb`, `cccc`) of the sender's signed
  record (§8.2) [M] R-04. The 4th (`dddd`) is not in the record and not a hash of any identifier the owner
  could name (emails, phone in every form, `altDsID`/`encDsID`, device name) [M] E1.4 run 2: an unlisted
  identifier or a filler.
- The tag is **not** `SenderIdentityAuthTag` (that changes per transfer on one BLE address) [M] R-09. Not
  `sha256(address)[:3]` [M] R-01. **It is a 3-byte Rapport auth tag** [H] S-13: `SipHash(IRK, msg)[:3]`
  (type 2/5 length is 3 bytes), which would rotate with the BLE address if the address is in `msg`. Test E1.3.
- The type-0x05 payload is not built in `Sharing.framework` (`SFBLEAdvertiser` builds only NearbyAction /
  NearbyInfo) [S] S-01; it is built in `sharingd` [S]; byte 0 is settled live (E1.3), the tag builder is not pursued (auth, §12).
- Receivers map short hashes to contacts: `-[SFBLEScanner _foundDevice:advertisementData:rssi:fields:]`,
  `contactHandlesForShortHashData`, `contactIDForEmailHash` [S].
- Privacy: the advert broadcasts 2-byte prefixes of the owner's phone/email hashes to anyone in range while the
  share sheet is open [M]; short enough to shortlist phone numbers [H] (PrivateDrop, USENIX Sec '21).

### 2.2 A second, smaller advert format (WirelessProximity)
`WPAWDL` (`+hashEmail:`, `+generateDataFromEmails:`, `startConnectionlessAWDLServiceAdvertisingWithData:`,
`useSmallerAirDrop`) builds a **10-byte** blob: `01`, then up to 3 × `SHA-256(UTF-8 email)[:3]` [S] S-01. Where it
is parsed by `-[SFBLEScanner foundAWDLDevice:rssi:]` (length 10 → device UUID) [S] S-18; when it is sent: **blocked [LIVE] E1.5**. Control [M]: no 10-byte `01 …` Continuity item from any address in the 7
sender-side btmon captures (`ble-airdrop.py --types`).

### 2.3 Receiver side
- Owly says a receiver wakes AWDL on an AirDrop advert if in Everyone mode or a hash matches a contact [H]
  (owly BLE-3). Which advert variants still wake iOS 27 (v1, v3 with zero hashes, none): **blocked [LIVE]** (E1.2, after E0.7b).

---

## 3. Link layer: AWDL

### 3.1 Frames
- Action frames: category 127, OUI `00:17:f2`, type 8; BSSID `00:25:00:ff:94:73`; subtype 0 = PSF, 3 = MIF [M] R-02
  (matches owly AWDL-1..4).
- **PSFs on ch 6** (the iPhone's master channel), MIFs on the channels it visits [M] R-02.
- Data frames: QoS data, LLC/SNAP `aa aa 03 00 17 f2 08 00`, AWDL header, then IPv6 [M] (owly AWDL-13).
- AWDL MAC: random, and it **lasts for days, not per session**: `02:00:00:00:00:0c` → `02:00:00:00:00:07` between
  20:02 and 21:15 on 11-02; `3a:b2:…` in all 33 captures 11-02…11-05; `02:00:00:00:00:0a` in all 38 captures
  11-08…11-16 [M] (all `air.pcap`, re-read 2026-11-16). The iPhone's `ReceiverPseudonym` (§9.2) changed in the same
  11-05…11-08 gap [M]. What triggers a change (restart, Airplane Mode, time): **blocked [LIVE]** (E2.7).

### 3.2 TLVs the iPhone sends
A lone iPhone (no AWDL peer) sends only 0x04, 0x05, 0x12, 0x18 [M] R-02. Once a peer exists it adds 0x07, 0x0c,
0x11, 0x21, 0x10 (arpa), then 0x15 Version and sometimes 0x06 [M] R-02.

| TLV | Name | iOS 27.2 value | Tag |
|---|---|---|---|
| 0x04 | Sync Params | master channel **6**, AW period 16 TU, next-AW channel = slot channel | [M] `probe/trace` |
| 0x05 | Election | metric 65 (`…00410000…`) | [M] |
| 0x06 | Service Params | `0000` (transfer), `0000004c0000000000` (idle ch 6) | [M] R-02 |
| 0x07 | HT caps | `00006f881bffff00000000000000009600010000` | [M] |
| 0x0c | Data Path State | flags `0xbf23` / `0x9f23`; country = AP's 802.11d country (`XX`); social 6/44/149; infra BSSID **zeroed**; infra channel = its Wi-Fi channel (36) or 0; full layout §3.3 | [M] R-05, S-20 |
| 0x10 | ARPA | hostname | [M] |
| 0x11 | 802.11 container | VHT Capabilities element `bf 0c 76f0b113 faff0000 faff0020`: cap-info `0x13b1f076` → max MPDU 11454, **supported channel width set = 1 (160 MHz)**, rx/tx MCS map `0xfffa` | [M] (decoded per IEEE 802.11 VHT Capabilities) |
| 0x12 | Channel Sequence | enc 3 (channel + op-class byte): `24 80` = 36/op-class 128, `95 80` = 149/128, `06 51` = 6/op-class 81; 16 slots; **slot 8 always ch 6**; ~6 different sequences in 25 s | [M] R-05 |
| 0x15 | Version | `a2 02` = **AWDL 10.2, device class iOS**. Other devices heard: `a2 01` (10.2, macOS class; DPS flag 0x0040 AirPlay sink set, also sends 0x13), `a0 02`, `93 02` (two iPhones; see §8.1 for how their requests differ) | [M] |
| 0x18 | Election v2 | master = self, distance 0, metrics 65/65 | [M] |
| **0x21** | **Preferred Channels** (`preferredChannels`) | `01 00 00 00 <infra channel, 2 bytes> 01 11 86 <txPower> 00 00 00 00`; infra bytes follow the iPhone's Wi-Fi: `24 81` (ch 36), `28 81` (ch 40), `0b 51` (ch 11), `64 80` (ch 100, a Mac), `00 00` (not joined) | [M] 85 captures, [S] S-19 |

Attribute IDs above 0x18 (none documented by owly/OWL/Wireshark): `wifip2pd` (Swift `CoreP2P.AWDLActionFrame`)
keeps a raw-value table for its `AWDLAttribute.AttributeType` enum whose first 16 entries are exactly the known IDs
0x00…0x18, followed by **0x1d `multiPeerBSSSteering`, 0x1e `ranging`, 0x1f `realtimeGroup`, 0x20
`extendedChannelSequence`, 0x21 `preferredChannels`, 0x22 `proMode`, 0x23 `airDrop`** [S] S-19. Payload fields
(Swift field descriptors) [S] S-19:
- 0x21 `PreferredChannels`: `infrastructureChannel` (Channel), `preferredChannels` [ `PreferredChannel`
  {`channel`, `txPower` Int8} ].
- 0x23 `AirDrop`: `version` (UInt8), `state` (enum `remove`, `add`). **Never sent by the iPhone** in any of our
  captures (sender and E0 sessions) [M].
- 0x1f `RealtimeGroup`: `flags`, `groupID` (MAC), `infrastructureChannel`, `infrastructureRSSI`, `additionalFlags`,
  `customRealtimeGroupInfraChannel`, `additionalServicesFlags`, 5 reserved bytes. 0x22 `ProMode`: `version`,
  `command`, `status` (UInt32), `flags`. 0x1d `MultiPeerBSSSteering`: `messageID`. Seen on air once, in
  `20261112-222213-rx-guard` (campus, a second iPhone with Version `93 02` present) [M]: **0x20**
  `00 00 86 00 11 00 04 08 02 0b 0b 00 00` from both that iPhone (3 frames) and ours (5 frames), and **0x22**
  `01 02 00 00 00 00 02 00 00 00` from the other iPhone (1 frame; by the field list: version 1, command 2, status 0,
  flags 2). 0x20 carries the same (channel 17, op-class 0x86) pair as 0x21's entry; the rest of its layout is [H].
  0x1d, 0x1e, 0x1f and 0x23 have never been seen [M] (85 captures).

TLV 0x21 = `PreferredChannels` layout (fields [S] S-19, bytes [M]):

| Bytes | Value | Field | Tag |
|---|---|---|---|
| 0–3 | `01 00 00 00` | header; byte 0 = 1 is likely a version (0x22/0x23 also start with `version`) | [H] |
| 4–5 | `24 81`, `28 81`, `0b 51`, `64 80`, `00 00` | **`infrastructureChannel`** = (channel, op-class): 36/0x81, 40/0x81, 11/0x51, 100/0x80; `00 00` exactly when DPS reports infra ch 0. Equal to the DPS infra channel in every capture | [M] E2.2 (answered from captures, below) + [S] |
| 6 | `01` | number of `PreferredChannel` entries (always 1 here) | [H] |
| 7–8 | `11 86` | entry `channel`: channel 17, op-class 0x86 = 134 (6 GHz, 160 MHz) | [M] constant; meaning fits `supports6GHz` (§3.3) |
| 9 | `00` (iPhones), `1a` (the Mac) | entry `txPower` (Int8): 0 or 26 | [M] value differs by device, which fits a per-entry field after the channel pair |
| 10–13 | `00 00 00 00` | reserved / unused | [H] |

The (channel, op-class) byte pair is the same encoding as TLV 0x12 enc 3 (`24 80`, `06 51`) [M]. **E2.2 is answered
without a new run** (2026-11-16 re-read of all `air.pcap`): the prediction "iPhone on a 2.4 GHz BSS → bytes 4–5 =
`0b 51`" holds in `20261114-225836-rx-base` and `-230004-rx-vht80` (403 + 260 frames, DPS infra channel 11), and
ch 40 gives `28 81` in the campus sessions. A device with Version `a0 02` and one with `93 02` (11-04) send no 0x21 at all [M].

### 3.3 Data-path-state (TLV 0x0c) layout and flags
Three sources agree: the bit names in Wireshark's AWDL dissector (`awdl.datastate.*`, public) [H→M], the Swift
field list of `CoreP2P.AWDLActionFrame.DataPathState` in `wifip2pd` (same order) [S] S-20, and an exact parse of
the iPhone's 47-byte value with those presence rules (every byte accounted for, counters move as named) [M] S-20.

| Bytes (iPhone) | Field | Present when | iPhone value |
|---|---|---|---|
| 0–1 | flags (LE) | always | `0x9f23` / `0xbf23` |
| 2–4 | country code | flag 0x0100 | `XX\0` |
| 5–6 | social channel map (bit0 ch 6, bit1 ch 44, bit2 ch 149) | 0x0200 | `0x0007` |
| 7–14 | infra BSSID (6) + infra channel (u16) | 0x0001 | BSSID zeroed, ch 36 |
| 15–20 | infra address | 0x0002 | = its AWDL MAC |
| — | AWDL address | 0x0004 | not sent |
| — | UMI (u16) | 0x0010 | not sent |
| 21–26 | unicast options: length u16 + bytes | 0x1000 | len 4, `00 00 00 00` |
| 27–28 | extension flags (u16) | 0x8000 | `0x197d` |
| 29–30 | log-trigger ID (`portUniqueLogID`) | ext 0x0001 | 0 |
| 31–34 | RLFC (`rootLoopFreeCounter`) | ext 0x0004 | 0x26f → 0x274 in 13.6 s |
| 35–38 | active time, ms since AWDL came up (`awdlOnSince`) | ext 0x0020/0x0040 | 18302 |
| 39–42 | AW sequence counter | 〃 | 1096 (≈ 18302 ms / 16.4 ms per AW) |
| 43–46 | AF update counter | 〃 | +7 in 13.6 s |

Flag bits (Wireshark name / `wifip2pd` Swift name): 0x0001 infra BSSID+channel, 0x0002 infra address, 0x0004 AWDL
address, 0x0008 RSDB, 0x0010 UMI, 0x0020 dual band (`supportsDualBand`), 0x0040 AirPlay sink (`isAirPlaySink`),
0x0080 follow channel sequence (`followChannelSequence`), 0x0100 country, 0x0200 social channel map, 0x0400 AirPlay
solo (`supportsAirPlaySolo`), 0x0800 UMI supported (`supportsUnicastMasterIndication`), 0x1000 unicast options,
**0x2000 real-time (`infraRealTimeActive`)**, 0x4000 rangeable (`isRangeable`), 0x8000 extension flags.
Extension bits: 0x0001 log-trigger ID, 0x0002 ranging discovery, 0x0004 RLFC, 0x0008 social-channel-map supported
(`supportsSocialChannelMap`), 0x0010 dynamic SDB (`supportsSimultaneousDualBand`), 0x0020 misc/counters, 0x0040 DFS
proxy (`supportsDFSProxy`), 0x0100 HE (`supportsHighEfficiencyRate`), 0x0200 sidekick hub (`isSidekickHub`), 0x0400
fast discovery, 0x0800 6 GHz (`supports6GHz`), 0x1000 ultra-low-latency infra (`supportsUltraLowLatencyInfra`),
0x2000 pro mode (`proModeActive`) [H→S name match].

iPhone 17: flags `0x9f23` = infra info, infra address, dual band, country, social map, AirPlay solo, UMI supported,
unicast options, extension; ext `0x197d` = log-trigger, RLFC, social-map supported, dynamic SDB, counters, DFS proxy,
HE, 6 GHz, ULL infra [M]. **Bit 0x2000 (real-time) is set while the iPhone's sequence is transfer-like** [M] R-05,
i.e. AirDrop puts the link in real-time mode during a transfer. The `unicastOptions` enum names the reasons a
peer requests a master change (`highThroughputStart/Stop`, `realtimeStartRequest`, `fastDiscovery`, AirPlay,
Sidecar, ranging, … 49 cases) [S] S-20.

### 3.4 Channel policy
- A sending iPhone on Wi-Fi ch 36 mixes 36, 149 and 6; it **leaves ch 36 for 10–14 s** at a time even on the same
  network [M] R-06. Seen sequences: `36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36`; `36,0,149,0,0,149,36,36,6,…`;
  `149,149,149,149,36,36,36,36,6,…`; `36,0,149,0,0,0,0,0,6,0,149,…` [M] R-05, R-06.
- Inputs to the choice, from the AWDL client API: a traffic registration per named service (`airdrop`, `airplay`,
  `sidecar`, … 26 names) with a peer, `preferredChannel`, `secondaryPreferredChannel`, options **bit 0 PreferInfra,
  bit 1 Background, bit 2 Alert**, flags `active`/`legacy`, and a `localization` (country) [S] S-17. Which values AirDrop registers is
  in code, not strings (not pursued); its visible effect is the DPS real-time bit 0x2000 during transfers [M] §3.3. Channel of a *receiving* iPhone (Everyone, on Wi-Fi ch 36) [M] E0.7b: `36,36,149,0,0,0,0,36,6,36,149,36,0,0,0,36`;
  ~32 s after a ch 6 peer appeared: `6,0,149,0,0,0,0,0,6,0,149,0,0,0,0,0` (ch 36 dropped, a 2nd ch 6 slot).
- Multicast is queued and sent once per channel-sequence cycle (16 slots × 64 TU = 1024 TU = 1.048576 s): the
  iPhone's mDNS packets are spaced at exact multiples of 1.048 s, and queued duplicates leave in one burst [M] R-12;
  "sent in slot 0" [H].

### 3.5 Rules for a Linux node (owly, all [H] unless noted)
AWDL-11 (be on a social channel), AWDL-12 (advertise only channels you serve; measured here: verbatim copying
under a pinned radio made the iPhone sync but never open TCP [M] R-11), AWDL-15 (ADDBA), AWDL-17 (valid peer
after MIF + Version).

---

## 4. Network layer
- IPv6 link-local on `awdl0`; the iPhone's address is `fe80::` + EUI-64 of its AWDL MAC
  (`02:00:00:00:00:07` → `fe80::38b2:6eff:fe79:2d40`) [M] R-12.
- No AAAA query seen from the iPhone: it uses the address from our answers' additional records [M] R-12
  (or the AWDL peer table [H]).

---

## 5. Discovery: mDNS on `awdl0`

### 5.1 iPhone as sender (measured, R-12)
- QM queries, often 3 in one packet: `_airdrop._tcp`, `_applicationServicePairing._tcp`, `_appSvcPrePair._tcp` [M].
  Apps on the phone also browse over awdl0 (`_spotify-connect._tcp`) [M].
- It announces `TestPhone._applicationServicePairing._tcp` and `TestPhone._appSvcPrePair._tcp` (device name in
  clear); it does not announce `_airdrop._tcp` while sending [M].
- Known-answer lists carry our PTR once known [M]. It sends SRV+TXT queries for our instance directly [M].
- 5 of 12 sessions had **no** iPhone mDNS at all and still connected (cached records, same instance name) [M].
- It searches in a burst when the share sheet opens, then backs off [M]; exact backoff schedule:
  **blocked [LIVE]** (E3.2).

### 5.2 Receiver records (what we publish; iPhone accepts it)
`<12 hex>._airdrop._tcp.local.` → SRV (any port works; Apple listens on 8770 [H] owly), AAAA = awdl0 link-local,
TXT `flags=139` (`0x8B`) [M] R-03. Apple receivers publish more TXT keys (~`0x37FF` flags) [H] Windfall.
The iPhone's own records as a receiver [M] E0.7b run 1: instance `<12 hex>._airdrop._tcp.local.` (probed with
`ANY?` first), **TXT `flags=111611`** (= `0x1B3FB`, the same value as its `DeviceSupportFlags`, §8.1), SRV
`<UUID>.local.:8770` where the UUID hostname is also its AWDL arpa name (TLV 0x10) and **rotates** (two values
32 s apart). So TXT `flags` and `DeviceSupportFlags` are one capability bitmask. Over all `send`/`find` dumps
(11-04…11-16): port 8770 in 27/27, `flags=111611` in every TXT we resolved (18/18), and a **new 12-hex instance
name every time AirDrop receiving starts** (16 distinct names in 27 records) [M].

### 5.3 Flags (= `DeviceSupportFlags`)
The receiver's TXT `flags` and the sender's `/Discover` `DeviceSupportFlags` are the same bitmask: 111611
(`0x1B3FB`) in both on this iPhone [M] E0.7b, R-04. Names: owly MDNS-4 / OpenDrop [H]; no Apple name strings [S] S-22.

| Bit | Name (owly/OpenDrop) | iPhone 17 | other Apple device (Windfall, `0x37FF`) | ours (`0x8B`) |
|---|---|---|---|---|
| 0x0001 | SupportsURLSharing | ✓ | ✓ | ✓ |
| 0x0002 | SupportsDVZip | ✓ | ✓ | ✓ |
| 0x0004 | SupportsPipelining | **–** | ✓ | – |
| 0x0008 | SupportsMixedTypes | ✓ | ✓ | ✓ |
| 0x0010, 0x0020 | unnamed | ✓ ✓ | ✓ ✓ | – |
| 0x0040 | SupportsIris | ✓ | ✓ | – |
| 0x0080 | SupportsDiscoverMaybe | ✓ | ✓ | ✓ |
| 0x0100 | unnamed | ✓ | ✓ | – |
| 0x0200 | SupportsAssetBundle | ✓ | ✓ | – |
| 0x0400 | unnamed | – | ✓ | – |
| 0x1000, 0x2000 | unnamed | ✓ ✓ | ✓ ✓ | – |
| 0x8000, 0x10000 | unnamed | ✓ ✓ | – – | – |

- iOS 27 with TXT `flags=0`: the receiver is listed and `/Discover` answered, but iOS never sends `/Ask`;
  `0x0A` is enough for a photo transfer [H] Windfall protocol-notes (iOS 27.0). Our `0x8B` works [M].
- The iPhone, unlike the Mac-class device, does not set SupportsPipelining: an iPhone receiver will not take an
  `/Upload` before its `/Ask` reply (owly UP-8) [H].
- Effect of the other bits on what the iPhone sends: **blocked [LIVE]** E3.3; bit meanings of the unnamed bits:
  **blocked [LIVE]** E5.6 (vary iPhone state, diff `DeviceSupportFlags`).

---

## 6. Security: TLS

| Item | iOS 27.2 | Tag |
|---|---|---|
| Connections | 2 per transfer (`/Discover`; `/Ask` + `/Upload`), sometimes an extra `/Discover` | [M] R-03, R-05 |
| Version | ClientHello offers 1.3 + 1.2 (GREASE); 1.3 negotiated with OpenSSL | [M] R-03 |
| SNI / ALPN | none / none | [M] |
| Groups | GREASE, **X25519MLKEM768**, x25519, P-256, P-384, P-521; key shares X25519MLKEM768 (1216 B) + x25519 | [M] |
| Ciphers | GREASE, TLS13 AES256-GCM, CHACHA20, AES128-GCM, then ECDHE GCM/CHACHA, legacy CBC | [M] |
| Fingerprint | JA4-like `t13i201100_a09f3c656075_7f0f34a4126d` (Network.framework default) | [M] |
| Server cert check | none: any self-signed receiver cert is accepted | [M] (our self-signed cert, every session) |
| Client cert | **only on request**: on the `/Ask` connection it presents `CN=com.apple.idms.appleid.prd.<altDsID>` (Apple Application Integration CA → Apple Root CA, RSA-2048, 13 months, EKU client+server, KU digitalSignature+keyAgreement, policies `1.2.840.113635.100.5.1`, `…100.5.7.1`, ext `…100.4.7`; intermediate `…100.6.2.3`) + CertificateVerify; on `/Discover` an empty Certificate; one connection resumed | [M] R-08 (TLS 1.2 probe) |
| Under TLS 1.3 with CertificateRequest | **blocked [LIVE]** E4.3 | [—] |

---

## 7. HTTP

### 7.1 Framing
| Request | Headers sent by iOS 27.2 | Tag |
|---|---|---|
| `POST /Discover` | `User-Agent: AirDrop/1.0`, `Connection: close`, `Transfer-Encoding: chunked` | [M] |
| `POST /Ask` | `User-Agent: AirDrop/1.0`, `Connection: keep-alive`, `Transfer-Encoding: chunked` | [M] |
| `POST /Upload` | `User-Agent`, `TotalBytes: <sum of FileSize>`, `Content-Type: application/x-dvzip`, `SenderPseudonym: pseud:<22 b64>`, `SenderPushToken: <64 hex>`, `TransferID: <UUID>`, `Connection: keep-alive`, `Transfer-Encoding: chunked` | [M] |

No `Host`, no `Content-Length` [M]. `SenderPseudonym` and `SenderPushToken` are **constant across transfers**
(per device/account); `TransferID` is new per transfer [M] (contact vs photos3). The server is in `sharingd`: a name
table `Ask`, `Upload`, `Discover` next to a `/%@` path format [S] S-03.

### 7.2 Message set
`sharingd` has one Swift enum `SDAirDropMessage` with exactly **7 request/response pairs** [S] S-21. Wire keys are
PascalCase strings next to each message's log text in `sharingd` [S]; [M] where a dump has them.

| Message | Request fields (wire keys) | Response fields (wire keys) | Seen |
|---|---|---|---|
| hello | `id` (TransferID), `FeatureFlags` (UInt32), `ContextType` (`unknown`, `peerPayment`) | `FeatureFlags`, `ContextType` | never [M] |
| discover | `SenderRecordData`, `DeviceSupportFlags` (UInt) | §8.3 | [M] |
| ask | §9.1 | §9.2 | [M] |
| upload | header `TransferID`, `TotalBytes`, `Content-Type`, body, `UploadComplete` (Bool?), `SenderPseudonym`, `SenderPushToken` | raw data | [M] (no `UploadComplete` seen) |
| exchange | `id`, `GivenName`, `FullName`, `Handle`, `VCardData`, `IdentityShareInfo` (an identity-share request) | same six fields | never [M]; E5.4 |
| identity share | `ReceiverAppleID`, `SenderRecordData`, `ReceiverDeviceIRKData`, `ReceiverEdPKData`, `ReceiverIDSDeviceID`, `RapportFeatureFlags` | raw data | never [M] |
| error | `id`, `error` (a Codable error) | raw data | never [M]; E5.1–E5.3 |

- *exchange* is a contact-card exchange after a transfer (fields of a contact + a `vcardData` blob); a receiver
  offers it with `SupportsContactExchange` in its `/Ask` reply (§9.2) [S]. Test: E5.4.
- *identity share* carries the same identity keys a `/Discover` reply can carry (§8.3) [S]; documented by name only
  (key material; not pursued, §12).

#### 7.2.1 Who sends what, when (sharingd log texts, S-26)
All four extra messages go **sender → receiver** on the transfer's connection; the receiver answers. The sender's
state machine, reconstructed from its log lines [S] S-26 and the 63 recorded sessions [M]:

```
DISCOVER ─► [IDENTITY-SHARE] ─► [HELLO] ─► ASK ─► (UserSafety check) ─► UPLOAD ─► [CONTACTS: SHARE CONTACT PERMISSION ─► EXCHANGE]
                                                              (UPLOAD SKIPPED for some types)        [ERROR on failure]
```

| Message | Sent when (gate, quoted log text) | Purpose | What a Linux peer should do |
|---|---|---|---|
| hello | only if the receiver supports it ("Receiver does not support HELLO, skipping"); also used to **pre-warm** a connection before the user picks a target ("Pre-warming Hello for endpoint", "Claiming pre-warmed connection", "Not pre-warming - too many existing connections") | capability handshake: request and response carry `FeatureFlags` (UInt32 option set; one option is named `.errorMessage`) and `ContextType` (`unknown` / `peerPayment`). "Hello response - peerPayment type is not supported by receiver device" → a sender checks the receiver can do Apple Cash Tap to Cash ("nearbyPeerPayment tap", "peerPayment type boop") | receiver: answering `400` is safe (sender skips). Sender: never send it (owly HTTP-4). Not one `/Hello` in 63 sessions [M]: the "supports HELLO" signal is not in our TXT/`/Discover` reply → hypothesis: one of the unnamed TXT `flags` bits (§5.3), test E5.7 |
| error | sender reports a failure to the receiver ("Sending ERROR request %s", receiver "Received ERROR request"); gated on the Hello reply: "No HELLO response present with .errorMessage support" | carry a `CodableError` (`id`, `error`); error names §7.2 list | a receiver that does no Hello will **never** get `/Error`: a cancel or failure reaches it as a plain TCP/TLS close (matches every failed session here and the E5.3 prediction) |
| identity share | after DISCOVER, only for a receiver that returned identity data and a contact match: "No receiver deviceIRKData or contactID or unsupported client - skipping IDENTITY-SHARE", "Same account send detected - skipping", "Finished DISCOVER and IDENTITY-SHARE" | an identity update between known contacts (Contacts Only path). Name level only (§12 boundary) | never sent to or by a peer without Apple identity; our `/Discover` reply carries no such data, so iOS skips it [M: never seen] |
| exchange | after the transfer, only if the receiver's `/Ask` reply said `SupportsContactExchange` and the share type calls for it: "Send StateMachine CONTACTS SKIPPED - {receiverSupportsContactExchange: %s, type: %s}", "CONTACTS SKIPPED - Not needed {senderContact, alwaysExchangeContacts, type}"; UI "Keep Contact Card?"; "Exchange Contact IDENTITY IMPORT SKIP. User chose to only receive" | **NameDrop** contact exchange: request = sender's card, response = receiver's card (same six keys). "SDAirDropNearFieldService: start NameDrop flow", "User has no NameDrop identity … myContactInfo". For a NameDrop-type send "UPLOAD SKIPPED": DISCOVER → ASK → EXCHANGE with no upload | an iPhone receiver answered `SupportsContactExchange: false` to us 12/12 [M], so it never offers it to a Linux sender in Everyone mode. Revised E5.4 prediction: setting the flag alone likely gives no `/Exchange` for a photo ("Not needed"); NameDrop needs the near-field (phone-to-phone tap) trigger |

Also from the same strings [S]:
- `IDSSessionID`, `ReceiverPseudonym`, `ReceiverPushToken` (`/Ask` reply) and `SenderPseudonym`, `SenderPushToken`
  (`/Upload` headers) feed the **Internet fallback** ("Setting Internet fallback endpoint with alias", "Sender extracted
  IDS session ID … from ask response"): a transfer that continues over the internet when the devices move apart. A
  Linux receiver omits them and transfers work [M]; the fallback is then simply unavailable.
- The receiver handles pipelining: "Received ASK request, proceeding pipelined with UPLOAD" vs "Waiting for ASK before
  UPLOAD"; `pipelinedUploadAvailable` (cf. TXT bit 0x0004, §5.3).
- Receive-side rejections that surface as a closed connection, not an HTTP error: "Screen is off, rejecting DISCOVER",
  "Peer is blocked, decline DISCOVER", "Request rejected, discoverability is OFF", "User did not accept request,
  invalidating connection", "Rate limited. Auto-declining queued transfers."
- AirDrop codes (iOS 26.2): "AirDrop Code Required" (`PIN_ENTRY_ALERT_TITLE`), and "AirDrop mode changed to Everyone
  … starting pairing service": the PIN path (`PairingPin` in `/Ask`) only runs in Everyone mode [S].
- Error names next to the messages: `No Files`, `System Sleep`, `Invalid File`, `Not Enough Space`, `Transfer Failed`,
  `File Not Readable`, `WiFi Disabled`, `WLAN Disabled`, `Authentication Failed` [S] S-03.
- URL paths of hello/exchange/identity share/error are not plain strings (Swift inline literals); the live
  path comes from E5.4 (the receiver dumps any unknown path). `/SharedIdentity` (owly) has no match in 27.2 [S].
- `sharingd` also has `TLS Verify Not Applicable to QUIC Connections` next to `SDAirDropSendConnection` [S]: a QUIC
  transport path exists; every iPhone transfer we saw used TLS over TCP [M].

---

## 8. `/Discover`

### 8.1 Request (iPhone → receiver)
| Key | Type | Value | Tag |
|---|---|---|---|
| `SenderRecordData` | data | Apple-signed validation record (§8.2), 3886 B, identical in every request | [M] R-04 |
| `DeviceSupportFlags` | int | `111611` (`0x1B3FB`) in all 34 requests from the iPhone 17, 11-02…11-14, home and campus | [M] R-04, dumps |

A second request form exists [M]: two iPhones whose AWDL Version TLV is `93 02` (`20261104-223904-e011i`, and a
stranger's phone in `20261112-225237-other-iphone`) sent `/Discover` with **only `SenderRecordData`** (no
`DeviceSupportFlags`), and an `/Ask` without `SenderComputerName` and without `SenderIdentityAuthTag`. A receiver
must treat all three as optional; ours does (both transfers were accepted).

Our sender (2026-11-04) sends `DeviceSupportFlags` = our receiver flags (`0x8B`), as every Apple sender sends its own;
before, its `/Discover` body was empty (QA test `test_linux_sender_discovers_like_an_iphone`).
`DeviceSupportFlags` is sent even in Everyone mode. It is Swift `deviceSupportFlags: UInt?` of
`SDAirDropMessages.DiscoverRequest`, logged as `DISCOVER request - senderRecordData: …, deviceSupportFlags: …`;
the class that builds it is `SDAirDropDeviceSupportManager` (ivars `deviceStatus: SDStatusMonitor`,
`nearbyAgent: SDNearbyAgent`) [S] S-22, so bits reflect device status. The same value is the receiver's TXT
`flags` (111611 in both) [M] E0.7b. The bits have no name strings [S]; their
meaning comes from varying the iPhone's state: **E5.6**. Its low bits `0x3FB` equal owly's "SupportsUPP example" [H].

### 8.2 `SenderRecordData` (validation record)
| Item | Value | Tag |
|---|---|---|
| Container | CMS SignedData, signer `CN=AppleIDValidationRecordSigning05` → Apple Application Integration 2 CA → Apple Root CA, with signingTime | [M] R-04 |
| Content | XML plist: `Version`=2, `altDsID` (46 chars), `encDsID` (= altDsID here), `ValidAsOf`, `SuggestValidDuration`=2592000 (30 d), `ValidatedEmailHashes` [2 × SHA-256 hex], `ValidatedPhoneHashes` [1 × SHA-256 hex] | [M] R-04 |
| Binding | `altDsID` = the CN suffix of the TLS client cert (§6) | [M] R-08 |
| Client model | `SFAppleIDIdentity`: `appleID`, `altDSID`, `encDSID`, `accountIdentifier`, `serialNumber`, `intermediateCertificateSerialNumber`, cert + key Keychain refs, expiry/validation dates | [S] |
| Hash normalization | SHA-256 (hex) of the identifier. Phone = **E.164 digits without `+`** (country code required; spaces/`+` ignored; national form does not match) [M] E1.4 runs 1–2. Email = the **lowercased** address (2/2 record email hashes reproduced; a capitalized input matches only after lowercasing) [M] E1.4 run 3; dots in the local part: untested (no dotted address) | [M] |

### 8.3 Response (receiver → iPhone)
What we send and the iPhone accepts: `ReceiverComputerName`, `ReceiverModelName`, `ReceiverMediaCapabilities`
(UTF-8 JSON bytes `{"Version": 1}`) [M] R-03. iOS builds this blob with PhotosFormats `PFMediaCapabilitiesQuery`; keys
`Version`, `Codecs`, `ContainerFormats`, `Vendor`(`com.apple`), `Profiles`, `LivePhotoFormatVersion`, `TranscodeChoice`,
`SortSourceEnumerationAscending`, `AssetBundleFormatVersion`, `SupportsAdjustmentBaseResources`, `CodecSupport`,
`HeifSubtypes`; support levels Unknown/Unsupported/Supported/SupportedMayBePlayable [S] S-16. A peer that
declares no HEIF/HEVC support gets transcoded media: with `{"Version": 1}` the iPhone converts photos to JPEG
(flat layout, §10.3); with the container and codec part of its own Version 3 blob (below) it sends `public.heic`
originals [M] (11-11…11-14; `airdrop config set media original`). Present `ReceiverComputerName` ⇔ visible [H] owly DISC-2.
Keys an Apple receiver may also send (vocabulary, Sharing.framework): `ReceiverRecordData`, `ReceiverEdPKData`
(Ed25519 public key), `ReceiverDeviceIRKData` (the 16-byte Rapport IRK that keys the peer's auth tags, S-13),
`RapportFlags`, `ReceiverID`, `IDSDeviceIdentifier`,
`VerifiableIdentity`, `SendersKnownAlias`, `ReceiverEmailHash`/`ReceiverPhoneHash`, `DiscoverableMode`,
`LegacyModeEnabled`/`Settable`, `UltraWideBandEnabled`, `RangingData`, `DeviceSupportsWAPI` [S].

**The iPhone's actual reply** to a Linux sender in Everyone mode [M] (37 `send --dump` replies, 11-05…11-16, two
iPhones): `200`, `Connection: keep-alive`, chunked, a binary plist with exactly three keys:

| Key | Value |
|---|---|
| `ReceiverComputerName` | device name |
| `IsAirDropable` | `true` |
| `ReceiverMediaCapabilities` | 1296 bytes of JSON, byte-identical in 36 replies from the iPhone 17 (1295 from the other iPhone): `Version` 3, `SupportsAdjustmentBaseResources` true, `Codecs` {`hvc1` {`Profiles`}, `CodecSupport` {`VTIsHDRAllowedOnDevice`, `VTCodecSupportDict`}}, `ContainerFormats` {`public.heif-standard` {`HeifSubtypes`}}, `Vendor` {`com.apple` {`LivePhotoFormatVersion`, `AssetBundleFormatVersion`}} |

There is **no `ReceiverModelName`** in the `/Discover` reply (it comes in the `/Ask` reply, §9.2), and none of the
identity keys above for a sender without an Apple identity.
`sharingd`'s own `DiscoverResponse` model [S] S-21: `mediaCapabilities`, `computerName`, `receiverRecordData`,
`modelName`, `deviceIRKData`, `edPKData`, `idsDeviceID`, `rapportFeatureFlags` (UInt64), `senderKnownAlias`,
`isAirDropable` (Bool) — wire `ReceiverMediaCapabilities`, `ReceiverComputerName`, `ReceiverRecordData`,
`ReceiverModelName`, `ReceiverDeviceIRKData`, `ReceiverEdPKData`, `ReceiverIDSDeviceID`, `RapportFeatureFlags`,
`SendersKnownAlias`, `IsAirDropable` [S] S-03. All optional: our 3-key reply is a valid subset [M].

---

## 9. `/Ask`

### 9.1 Request (measured R-04, R-07)
| Key | Type | iOS 27.2 | Tag |
|---|---|---|---|
| `TransferID` | dict `{id: UUID string}` | new per transfer; repeated as `/Upload` header | [M] |
| `TransferType` | dict | `{files: {}}` or `{links: {}}` | [M] |
| `SenderComputerName` | string | device name (`TestPhone`) | [M] |
| `SenderModelName` | string | `"iPhone"` (not a model id) | [M] |
| `SenderID` | string | 12 hex (6 bytes), constant per device (`0a1b2c3d4e5f`) | [M] |
| `BundleID` | string | the sharing app: `com.apple.mobileslideshow` (Photos), `com.apple.mobilephone` (contact), `com.apple.mobilesafari`, `com.reddit.Reddit` | [M] |
| `ConvertMediaFormats` | bool | `false` in all 7 shares | [M] |
| `Items` | array | URLs (link share); `[]` for files | [M] |
| `Files` | array of dict | `FileName`, `FileType` (UTI), `FileSize`, `FileBomPath` (`./<name>`), `FileIsDirectory`, `ShouldConvertMediaFormats`; `[]` for links | [M] |
| `FileIcon` | data | JPEG 2000 preview, 14–82 KB; absent for third-party links | [M] |
| `SenderRecordData` | data | same blob as `/Discover` | [M] |
| `SenderIdentityAuthTag` | data | **3 bytes, new per transfer** | [M] R-05, R-09 |

- `SenderIdentityAuthTag` is the wire name of Swift `senderIdentityAuthTag: Data?`, a parameter of the
  `SFAirDrop` send request next to `pairingPin` / `customPayload`, and a field of the receiver's
  `SFAirDropReceive.AskRequest` next to `verifiableIdentity` [S] S-02. It belongs to the **AirDrop PIN pairing**
  feature (`SFPinPairingRequest`, events `AutoAccepted`/`PinDiscovered`/`ProgressUpdate`, `StartTempPairing`) [S].
  It is **not** the device-setup `targetAuthTag`/`resumeAuthTag` [S] S-02. **Derivation (algorithm): a 3-byte
  Rapport auth tag** `SipHash(IRK, msg)[:3]` [H] S-13 — the 3-byte length matches Rapport type 2 (device IRK) or
  type 5 (contacts-ratcheted IRK); the contacts-scoped IRK fits "prove identity to a contact", and the receiver's
  `ReceiverDeviceIRKData` in `/Discover` is the verifying key. The exact type and the `msg` bytes are not pursued
  (auth-tag derivation is outside this spec, §12); whether a sender must send it: Q-T3.
- **Complete receive-side `/Ask` field set** — the fields `SFAirDropReceive.AskRequest` decodes, in order, from the
  Swift field descriptor [S] S-15: `type`, `transferTypes`, `canAutoAccept`, `verifiableIdentity`, `senderIsMe`,
  `contactIdentifier`, `senderBundleID`, `senderComputerName`, `senderModelName`, `senderEmail`, `senderEmailHash`,
  `senderCompositeName`, `senderFirstName`, `senderLastName`, `senderIcon`, `smallPreviewImage`, `previewImage`,
  `itemsDescription`, `itemsDescriptionAdvanced`, `urlItems`, `items`, `customPayload`, `pairingPin`,
  `senderIdentityAuthTag`. (camelCase = the receiver's decoded model; maps to the PascalCase wire keys.) So a
  receiver accepts more than the 10 keys our sessions carried; whether an Apple *sender* ever puts the extras on
  the wire is E6.x. Other vocabulary strings elsewhere in Sharing: `SendersKnownAlias`, `SenderPhoneHash`,
  `SmallFileIcon`, `FileSubType`, `ExtractLivePhotoImages`, `ExtractPhotosBundleMedia`, `UsePKZip`,
  `MediaCapabilities`, `SupportedMedia` [S].

- Sender-side model (`SDAirDropMessages.AskRequest`) [S] S-21: `id`, `type`, `senderRecordData`, `senderID`,
  `senderBundleID`, `computerName`, `modelName`, `urlItems`, `files` (`fileName`, `fileType`, `fileSubType`,
  `fileSize`, `fileBOMPath`, `fileIsDirectory`, `shouldConvertMediaFormat`), `itemsDescription`, `fileIcon`,
  `smallFileIcon`, `shouldConvertMediaFormat`, `customPayload`, `pairingPin`, `senderIdentityAuthTag`; wire keys
  `TransferID`, `TransferType`, `SenderRecordData`, `SenderID`, `BundleID`, `SenderComputerName`, `SenderModelName`,
  `Items`, `Files`, `ItemsDescription`, `FileIcon`, `SmallFileIcon`, `ConvertMediaFormats`, `CustomPayload`,
  `PairingPin`, `SenderIdentityAuthTag` [S]. Not yet seen from the iPhone: `ItemsDescription`, `SmallFileIcon`,
  `CustomPayload`, `PairingPin`, `FileSubType` [M].

### 9.2 Response and timing
- `200` with `ReceiverComputerName`, `ReceiverModelName` accepted [M] R-03.
- Full response model [S] S-21: `computerName`, `modelName`, `supportsContactExchange` (Bool?), `idsSessionID`,
  `pseudonym`, `pushToken`, `customPayload` — wire `ReceiverComputerName`, `ReceiverModelName`,
  `SupportsContactExchange`, `IDSSessionID`, `ReceiverPseudonym`, `ReceiverPushToken`, `ReceiverCustomPayload` [S].
- **The iPhone's actual reply** when its user accepts [M] (23 replies, 11-05…11-16): `200`, keep-alive, chunked,
  354-byte binary plist with six keys: `ReceiverComputerName`, `ReceiverModelName` (`"iPhone"`),
  `SupportsContactExchange` **false** (23/23), `IDSSessionID` (a new UUID per transfer), `ReceiverPseudonym`
  (`pseud:` + 22 base64 chars; one value 11-05, another 11-08…11-16), `ReceiverPushToken` (64 hex, constant per
  device). No `ReceiverCustomPayload`. Its `/Upload` reply is `200` with `Content-Length: 0` (20/20).
- Our sender's `/Ask` carries **no `SenderIdentityAuthTag`, no `SenderRecordData` and no `FileIcon`**, and the iPhone
  shows the prompt and accepts (23 accepted, files up to 417 MB) [M]. In Everyone mode none of the three is required.
- A decline, discoverability off or rate limiting reaches a sender as a **closed connection with no HTTP reply**
  [M] (send logs; QA test `test_iphone_closes_connection_on_ask_reads_as_declined_not_a_failure`).
- iOS times the `/Ask` round trip; a receiver should answer `200` before reading the body ("early ask") [H]
  Windfall; used here in every successful session [M]. Decline/timeout behaviour: **blocked [LIVE]** (E5.1, E5.2).

---

## 10. `/Upload` and payload

### 10.1 Upload
- Same TLS connection as the accepted `/Ask`, same `TransferID` [M] R-07.
- `TotalBytes` = sum of `FileSize` (130; 2509294), not the body length (photo body 2488587 B) [M] R-07.
- **iOS withholds the final empty chunk until the receiver replies**; reply when `TRAILER!!!` has been read [M]
 .

### 10.2 DVZip
Blocks `[4-byte BE header][payload]`; header bit 31 = stored, low 31 bits = length; otherwise one zlib stream per
block [M] (our decoder reads every iOS 27.2 upload). Apple block size 64 KiB [H] owly.

### 10.3 Archive
cpio odc (`070707`), ends with `TRAILER!!!`. iOS 27.2 uses **three layouts**, chosen by how Photos prepared the
item [M] (56 `/Ask` dumps, 117 file entries; corrected 2026-11-16, the earlier "no `NSIRD_` on iOS 27.2" came from
the first five shares only):

| Layout | `FileBomPath` and archive members | Seen for |
|---|---|---|
| flat | `.` (040755), `./<FileName>` (0100644) | contact, a JPEG or video sent while we declared no HEIF support, PNG, everything our own sender builds |
| staging folder | `./Containers/Data/PluginKitPlugin/<UUID>/tmp/NSIRD_AirDrop_<6 chars>/<FileName>` with every parent as a directory member (`.` 040711, parents 040755, the `NSIRD_` folder 040700), **one `NSIRD_` folder per item**, and usually a **`._<FileName>` AppleDouble sidecar** (1.7–1.9 KB) beside it | photos sent as originals (`public.heic`, `public.jpeg`) once we declare HEIF support (§8.3) |
| conversion folder | `./Media/PhotoData/OutgoingTemp/PHMediaFormatConversionRequest-<UUID>/<FileName>`, parents as directory members, no sidecar | videos in the same share |

One share can mix the last two (25 items: 24 photos + 1 video, `20261114-230004-rx-vht80`). The sidecar is not
listed in `Files` and not counted in `TotalBytes` (150 868 768 = the sum of the 25 `FileSize` values). The same
file name can occur twice in one share, in different `NSIRD_` folders. A receiver should save each `Files` entry
by its `FileBomPath` and drop the folders and sidecars (ours does: QA tests
`test_iphone_sends_several_photos_at_once_and_they_land_flat`, `…from_different_staging_trees`).

### 10.4 Share-type matrix
| Share | BundleID | TransferType | Files / Items | FileType | FileIcon | Tag |
|---|---|---|---|---|---|---|
| Safari link | `com.apple.mobilesafari` | links | Items [url] | – | 58 KB | [M] R-04 |
| Reddit link | `com.reddit.Reddit` | links | Items [url] | – | none | [M] R-07 |
| Contact | `com.apple.mobilephone` | files | `./<Name>.vcf` 130 B | `public.vcard` | 14 KB | [M] R-07 |
| Photo (JPEG) | `com.apple.mobileslideshow` | files | `./IMG_1571.jpg` | `public.jpeg` | 82 KB | [M] R-07 |
| Video | `com.apple.mobileslideshow` | files | `./IMG_1515.MOV` 31.2 MB | `com.apple.quicktime-movie` | 55 KB | [M] R-07 |
| Photo (HEIC original) | `com.apple.mobileslideshow` | files | staging-folder layout (§10.3), `ShouldConvertMediaFormats` true on most entries, top-level `ConvertMediaFormats` false | `public.heic` | yes | [M] 11-12…11-14 (50 entries) |
| PNG | `com.apple.mobileslideshow` | files | `./<name>.png` 11.9 MB | `public.png` | yes | [M] `…-223904-e011i` |
| Multi-photo + video | `com.apple.mobileslideshow` | files | 25 entries in one `/Ask`, one upload, mixed layouts | `public.heic`, `public.jpeg`, `com.apple.quicktime-movie` | yes | [M] `…-230004-rx-vht80` |
| Live Photo, album, folder, PDF, Note, Maps, Wallet pass, contact with photo, > 4 GB, other apps' links | **blocked [LIVE]** E6.x | [—] | [—] | [—] | [—] |

Our own sender uses `BundleID` `com.apple.finder`, the flat layout and no `FileIcon`; the iPhone accepts files and
links in that form [M] (23 sends).

---

### 10.5 Throughput (one radio pinned to ch 149) [M]
Rewritten 2026-11-16; the earlier "~1.5 % duty cycle, airtime-bound" reading was wrong (it divided bytes by the PHY
rate; corrected in EXPERIMENTS P2.2–P2.7). What the measurements say about the protocol:

- A sending iPhone on 5 GHz Wi-Fi is on social channel 149 for 20–30 % of the time (P2.5); a receiving one gives
  a sender about 69 % of slots (E-TXAGG). That presence, not the PHY rate, sets the ceiling for a peer that sits on
  one channel.
- The iPhone asks its peer for a **Block Ack session** (ADDBA) and aggregates once it is answered: receive went
  1.4 → 3.2 MB/s (P2.3). It **grants** one when asked in AWDL's frame format while a transfer runs, not before
  (E-TXAGG): send went 5.4 → 30.6 MB/s.
- It uses what the peer's AWDL TLVs declare: 2 streams (HT caps), 40 MHz, then VHT MCS 9 at 80 MHz when TLV 0x11
  carries VHT capabilities: receive 21.0 MB/s (E-VHT80).
- The DPS real-time bit and a presence window in our own frames raise the share of slots it gives us (E2.10, P2.4).
- Current figures: iPhone → Linux 21.0 MB/s (150.9 MB, 25 items), Linux → iPhone 30.6 MB/s (416.8 MB)
  (`docs/FINDINGS.md`).

## 11. Identity and modes
- Everyone for 10 Minutes works without an Apple identity on the receiver [M] (every session). iOS reverts after
  10 minutes [H] owly ID-6.
- Contacts Only: the record proves "altDsID owns these hashes" and the TLS client cert proves "this peer holds
  altDsID's key" [M] R-08. How an Apple receiver checks the record against its contacts: not pursued (verification internals are outside
this spec, §12); observable behaviour: Q-T2 (E0.7d).
- AirDrop PIN pairing ("AirDrop codes", iOS 26.2+) exists client-side [S] S-02; on the wire it is the `/Ask` key
  `PairingPin` (+ `SenderIdentityAuthTag`) [S] S-21. Its auth internals are not pursued (§12).

---

## 12. Open questions

Every item is **answered** (evidence cited), **blocked [LIVE]** on a hardware-in-the-loop run only the operator
can do (exact command in `docs/EXPERIMENTS.md` under the E-id; nothing else blocks it).
Rows that touch authentication are asked in their **interop form** (what a client must send or can expect,
observable on the wire). How Apple derives or verifies tags, keys and records is not a question of this spec
(operator boundary, 2026-11-04) and is documented only at the mechanism level (S-13, R-08). [DMG] no longer
exists: the FileSystem DMG was extracted (S-00), and S-19…S-22 used it.

Where: **home** = shared on ch 36 (iPhone sends); **ch 6** = AWDL on ch 6: at home with `--mode borrow` (Wi-Fi
paused for the session) or a ch 6 router band, or at campus (E0.7 setup); **any** = no radio.

| ID | Question | Disposition |
|---|---|---|
| Q-B1 | Which identifier is the 4th BLE hash `dddd`? | **answered (negative)** [M] E1.4 run 2: not any of the account's 2 record emails, its phone (any form), 2 more addresses, `altDsID`/`encDsID`, device name, model or `SenderID`. It is an identifier not listed in Settings or a filler. A sender may put anything there: our zero hashes already work for Everyone [M]. |
| Q-B2 | Byte `0x40` and the 3-byte tag: what must a sender put there? | tag: 3 bytes, constant per BLE address, not the per-transfer tag [M] R-09; mechanism [H] S-13. Byte `0x40` vs mode: **blocked [LIVE] E1.3** (any). Does a receiving iPhone wake for a random tag / no tag: **blocked [LIVE] E1.2** (ch 6; our v3 advert has a random tag, v1 has none). |
| Q-B3 | Hash normalization (owly OQ 1) | **answered**: phone = SHA-256 of E.164 digits without `+`; email = SHA-256 of the lowercased address [M] E1.4 runs 1–3 (all 3 record hashes reproduced). |
| Q-B4 | Which advert variants wake iOS 27 | v3 zero-hash: a receiving iPhone announced `_airdrop._tcp` 1.08 s after our advert started [M] e07b2 (no control yet). v1 / none: **blocked [LIVE] E1.2** (home in borrow mode; `--ble-version 1/off`). |
| Q-B5 | Where the WPAWDL 10-byte advert is used | **answered (builder + consumer)** [S] S-01/S-18; not sent while sharing [M] (7 captures). Trigger: **blocked [LIVE] E1.5** (any). |
| Q-A1 | TLV 0x21 layout | **answered**: name `preferredChannels` and fields {`infrastructureChannel`, [`channel`, `txPower`]} [S] S-19; bytes 4–5 = infra channel over five values incl. 2.4 GHz (`0b 51`) [M] §3.2 (E2.2, from captures); bytes 7–8 constant, byte 9 differs by device (`txPower`) [M]. Header bytes 1–3, the count byte and the tail keep their [H] names: nothing we can vary changes them. |
| Q-A2 | DPS flag 0x2000 and the other bits | **answered**: full layout and bit map §3.3 [S] S-20 + byte-exact parse [M]; 0x2000 = real-time (`infraRealTimeActive`), set during transfers [M] R-05. |
| Q-A3 | Channel of a receiving iPhone; widen/narrow | receiving sequence **answered** [M] E0.7b (§3.4), incl. one widening step toward a ch 6 peer. Systematic widen/narrow: **blocked [LIVE] E2.5** (ch 6). |
| Q-A4 | Election with a Linux peer | PSF-on-ch6 / MIF roles **answered** [M] R-02. Outcome vs our metric: **blocked [LIVE] E2.6** (home; OWL `-M`, reinstall). |
| Q-D1 | iPhone's TXT record and `/Discover` reply as a receiver | **answered** [M]: TXT §5.2 (`flags=111611`, UUID SRV host, port 8770, new instance name per start); `/Discover` reply §8.3 (3 keys, 37 replies); `/Ask` and `/Upload` replies §9.2. |
| Q-D2 | mDNS backoff schedule | burst + 1.048 s cycle **answered** [M] R-12; curve: **blocked [LIVE] E3.2** (home). |
| Q-D3 | Effect of our TXT flags and `ReceiverMediaCapabilities` | media capabilities **answered** [M] §8.3 (no HEIF declared → JPEG; HEIF + HEVC declared → HEIC originals in staging folders); the Live Photo / asset-bundle keys: E6.x. TXT flags: **blocked [LIVE] E3.3** (`AIRDROP_TXT_FLAGS`). |
| Q-T1 | Client cert under TLS 1.3 | TLS 1.2 **answered** [M] R-08; TLS 1.3: **blocked [LIVE] E4.3** (home, `AIRDROP_TLS_PROBE=13`). |
| Q-T2 | Contacts Only: what does a receiving iPhone show a non-contact Linux sender? | sender-side chain **answered** [M] R-08. Observable receiver behaviour (TXT, `/Discover` reply or none, listed or not) for a sender without an Apple ID: **blocked [LIVE] E0.7d** (ch 6). |
| Q-T3 | `SenderIdentityAuthTag`: must a sender send it? | 3 bytes, new per transfer [M] R-05; mechanism [H] S-13; our receiver ignores it and every transfer works [M]. **Answered: not required in Everyone mode** — our sender omits it (and the record) and the iPhone accepted 23 of our `/Ask` requests [M] §9.2; two other iPhones omit it too (§8.1). |
| Q-H1 | Other messages (hello, exchange, identity share, error) | **answered (fields + wire keys)**: §7.2 [S] S-21; **direction, gates and purpose answered** §7.2.1 [S] S-26 (hello = capability handshake / pre-warm, error = sender failure report gated on Hello's `.errorMessage`, identity share = contacts-only identity update, exchange = NameDrop card swap). URL paths + live use: **blocked [LIVE] E5.7** (Hello via TXT flags) and **E5.4** (home, `AIRDROP_CONTACT_EXCHANGE=1`). |
| Q-H2 | Cancel / decline / timeout on the wire | **blocked [LIVE] E5.1, E5.2, E5.3** (home; `AIRDROP_ASK_STATUS`, `AIRDROP_ASK_DELAY`). |
| Q-H3 | Resume of an interrupted transfer | **blocked [LIVE] E5.5** (home). |
| Q-H4 | `DeviceSupportFlags` bits and origin | origin **answered**: `SDAirDropDeviceSupportManager`, `UInt` [S] S-22; **same bitmask as the receiver TXT `flags`** (111611 in both) [M] E0.7b; 10 bits named via owly/OpenDrop [H] (§5.3). The value never changed in 34 requests and 18 TXT records over two weeks, two sites, sending and receiving [M], which points to static capabilities. Unnamed bits (0x10, 0x20, 0x100, 0x1000, 0x2000, 0x8000, 0x10000): **blocked [LIVE] E5.6**. |
| Q-P1 | Share-type matrix | 8 types **answered** [M] R-07 + §10.3/§10.4 (HEIC originals, PNG, 25-item mixed share, three archive layouts); rest: **blocked [LIVE] E6.x**. |
| owly OQ 7 | Record binds to TLS? | **answered**: client cert CN = altDsID [M] R-08. |
| owly OQ 12 | Link share form from an Apple sender | **answered** [M] R-04. |
| owly OQ 13 | What `SenderIdentityAuthTag` is | **answered**: a 3-byte per-transfer tag tied to AirDrop PIN pairing, a Rapport auth tag [M] R-05 + [S] S-02/S-13. Not required in Everyone mode (Q-T3). |

**Summary (2026-11-16):** 23 rows. Fully answered: Q-A1, Q-A2, Q-B1 (negative), Q-B3, Q-D1, Q-T3, OQ 7, OQ 12,
OQ 13. Answered with a live remainder: Q-B2, Q-B5, Q-A4, Q-D2, Q-T1, Q-H1, Q-H4, Q-P1. Live only: Q-B4, Q-A3, Q-D3,
Q-T2, Q-H2, Q-H3. All live runs now work at home in the default mode (borrow, ch 149); the batches and commands are
in `docs/EXPERIMENTS.md` "Ready experiments". Where a row below still says "home", "ch 6" or "shared mode", read
"the default `airdrop receive` / `send`".

---

## 13. Implementer notes (Linux node)
1. Wake: a BLE type-0x05 v3 advert while browsing; non-connectable is what iOS itself sends [M].
2. Be where the iPhone is: a sending iPhone visits its infra channel + 149 + 6 [M]; a receiving one sits on its
   social channels [H]. A radio pinned to the router's channel must advertise only the slots it serves (OWL `-I`) [M] R-11.
   A P2P-GO beside a connected station on the MT7921 does **not** receive on the GO's channel [M] E0.3b. What
   works with one radio: pause the station for the session and run a hidden P2P-GO alone on 149 (`--mode borrow`,
   the default here), with Wi-Fi scans held off for the session (they took the radio off channel 70 % of the
   time, P2.7). Answer the iPhone's ADDBA, declare HT 2-stream and VHT 80 MHz capabilities in the AWDL TLVs, and
   set the DPS real-time bit (§10.5).
3. Find the receiver by mDNS, or `/Discover` each AWDL neighbour on port 8770 (owly DISC-5) [H].
4. `/Ask` in the iOS 27.2 form with `TransferID`; `/Upload` with the same id, DVZip; answer `/Ask` at once, reply
   to `/Upload` after `TRAILER!!!` [M].
5. Receiving: treat `DeviceSupportFlags`, `SenderComputerName`, `SenderIdentityAuthTag` and `FileIcon` as optional
   (§8.1); expect three archive layouts and `._` sidecars (§10.3); declare HEIF/HEVC in
   `ReceiverMediaCapabilities` to get originals instead of converted JPEGs (§8.3).
6. Sending: `DeviceSupportFlags` in `/Discover`; an `/Ask` with name, model, `SenderID`, `BundleID`, `TransferID`,
   `TransferType`, `Files`/`Items` is enough in Everyone mode; a closed connection after `/Ask` means declined (§9.2).

## 14. Tools in this repo
| Tool | Gives |
|---|---|
| `airdrop receive --dump DIR` | every request: headers, decoded plist (JSON), previews (`.jp2`), records (`.der`), archive members, TLS facts |
| `airdrop send/find --dump DIR` | the receiver's side: replies, TXT records, TLS facts |
| `scripts/airdrop-trace.sh [send\|find] …` | one session: awdl0 + 802.11 pcaps, btmon, dump, OWL log |
| `scripts/awdl-decode.py PCAP [--all] [--tlv 0x21]` | AWDL TLVs per node / one TLV over time |
| `scripts/tls-hello.py PCAP` | ClientHello/ServerHello, JA4-like, certificates |
| `scripts/ble-airdrop.py BTMON.txt` | type-0x05 adverts, tag/address/hash changes |
| `scripts/dump-diff.py DUMPS…` | key × session matrix |
| `scripts/hash-check.py RECORD.der PREFIX…` | which identifier + normalization gives each contact hash |

New iOS version: run `airdrop receive --dump` and a trace, diff the JSON and TLVs against this document.
Redact device names, previews and records before sharing dumps.
