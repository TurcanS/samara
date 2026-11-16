# The MT7921 driver patch

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

## Other cards

Nothing here is needed for a card whose driver already sends injected frames at the requested rate. Only the
MT7921 has been tested.
