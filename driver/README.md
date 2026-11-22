# Driver patches

Two cards have a patch: the MediaTek MT7921 (`mt7921-awdl.diff`) and the Realtek RTL8852BE
(`rtw89-awdl.diff`). Card pages with measurements and troubleshooting are in [`docs/wiki/`](../docs/wiki/Home.md).

## MediaTek MT7921 / MT7922

`mt7921-awdl.diff` is one patch against the kernel's own `mt7921e` (PCIe MT7921/MT7922), written against
and tested on Linux 7.2.5. It touches three files in `drivers/net/wireless/mediatek/mt76/mt7921/`
(`pci_mac.c`, `pci.c`, `mt7921.h`), about 310 added lines. It is GPL-2.0, like the code it changes.

Build and load:

```sh
scripts/mt7921-build.sh          # fetches the mt76 source for your kernel (about 300 MB), patches, builds
scripts/mt7921-ratefix.sh load   # swaps the module in until the next reboot; Wi-Fi blips for ~3 s
scripts/mt7921-ratefix.sh revert # or just reboot
```

Nothing is installed. The stock module on disk is never touched, and a module built for another kernel is
refused before anything is unloaded.

## What it changes

Normal Wi-Fi traffic takes the same code path as before. The patch acts on frames that user space injects
through a monitor interface, and on stations that user space registered on our own access-point interface.

1. **Injected frames keep the rate the sender asked for.** The stock driver clears the rate of an injected
   unicast frame and the firmware falls back to 1 Mbit/s CCK. That rate does not exist on 5 GHz, so the frame is
   never transmitted. This part comes from
   [shaecodes-wilde/mt76](https://github.com/shaecodes-wilde/mt76), branch `fix/mt7921-family-driver`.
2. **Injected HT rates.** A radiotap MCS field (MCS 0-15, 20 or 40 MHz, short guard interval) is honoured the
   same way. Measured: 43 MB sent at 1.9 MB/s against 1.0 MB/s at 54 Mbit/s, and 5.6 MB/s once our own scans were stopped (`docs/EXPERIMENTS.md`, "P2.7").
3. **Block Ack towards AWDL peers** (module parameter `awdl_ba`, off by default; the helper switches it on for
   one sending session and restores it). OWL negotiates the Block Ack session itself, in AWDL's frame format,
   which mac80211 neither sends nor accepts. The driver only watches for the peer's answer: an ADDBA response
   with status 0 from a registered station switches the firmware's aggregation on for that station, a DELBA
   switches it off. Injected QoS data without a rate then goes out with the firmware's own rate control.
   Measured: 5.4 → 30.6 MB/s to an iPhone 17 (`docs/EXPERIMENTS.md`, the "E-TXAGG" entries).

`airdrop doctor` reports which build is loaded. Without the patch, replies to one device do not go out on 5 GHz at all (see 1).

## Realtek RTL8852BE (rtw89)

`rtw89-awdl.diff` is one patch against the kernel's own rtw89 driver, written against and tested on Linux
7.3.5. It touches `core.c` and `core.h` in `drivers/net/wireless/realtek/rtw89/`, about 160 added lines. It is
GPL-2.0, like the code it changes.

```sh
scripts/rtw89-build.sh          # fetches the rtw89 source for your kernel, patches, builds the 8852BE stack
scripts/rtw89-load.sh load      # swaps the five modules in until the next reboot; Wi-Fi blips for a few seconds
scripts/rtw89-load.sh revert    # or just reboot
```

Nothing is installed: the modules on disk are untouched, and a build for another kernel is refused before
anything is unloaded. The five modules (`rtw89_core`, `rtw89_pci`, `rtw89_8852b_common`, `rtw89_8852b`,
`rtw89_8852be`) are built from one tree and always swapped together: the patch adds a field to
`struct rtw89_dev`, and without MODVERSIONS a mix of ours and the distribution's would go unnoticed. Built
without the patch, the same tree reproduces Arch's 7.3.5 modules byte for byte (same symbols, same `.text`).

**The stock driver already works** once the iPhone is a station of our access point, which the helper does in
every session on rtw89: send 5.5 MB/s, receive 20.5 MB/s. Without the station entry the chip sends frames for an
unknown peer under the access point's own entry, 17 at a time every ~140 ms (0.2 MB/s).

**What the patch adds**, all behind the module parameter `awdl_ba` (off by default; the helper switches it on for
a session and restores it). With it off, the module behaves like the stock one.

1. **Block Ack towards AWDL peers.** OWL negotiates the session in AWDL's frame format, which mac80211 neither
   sends nor accepts. The driver watches for the peer's answer: an ADDBA response with status 0 from a registered
   station switches the firmware's aggregation on for that station and TID (what mac80211's
   `IEEE80211_AMPDU_TX_OPERATIONAL` does), a DELBA from the recipient switches it off. The kernel log shows
   `awdl_ba: <mac> tid N: aggregation on`.
2. **No mac80211 sessions for injected frames.** Stock rtw89 starts mac80211's own Block Ack session for every
   station it sends to; the iPhone never answers it, and the queue stops for up to a second each time.
3. **Firmware rate control for injected frames without a rate**, instead of reading the empty radiotap field as
   a fixed rate.

Measured with an iPhone 17 (417 MB video each way): **send 40.5 MB/s** at VHT MCS 9, 80 MHz, two streams, no
retries; receive 25.2 MB/s (`docs/EXPERIMENTS.md`, "E-RTW89").

## Other cards

Nothing here is needed for a card whose driver already sends injected frames at the requested rate and keeps
them moving. [`docs/wiki/Faster-on-your-card.md`](../docs/wiki/Faster-on-your-card.md) shows how to find out what
holds a card back, with the two cases above as examples.
