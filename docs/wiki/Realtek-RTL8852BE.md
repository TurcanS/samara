# Realtek RTL8852BE (`rtw89`)

| | |
|---|---|
| Chip | Realtek RTL8852BE, Wi-Fi 6, 2x2, PCIe (`10ec:b852`) |
| Driver | `rtw89_8852be` (stack: `rtw89_core`, `rtw89_pci`, `rtw89_8852b_common`, `rtw89_8852b`, `rtw89_8852be`) |
| Tested | Arch Linux, kernel 7.3.5, NetworkManager; iPhone 17, iOS 27.2 |
| Stock driver | works: receive 20.5 MB/s, send 5.5 MB/s |
| With our patch | **receive 25.2 MB/s, send 40.5 MB/s** (VHT MCS 9, 80 MHz, two streams, no retries) |

`lspci -nnk | grep -A3 -i network` shows whether you have it: `Kernel driver in use: rtw89_8852be`.

## Setup

1. Install Samara (README, "Install"). `airdrop doctor` should show a line for `rtw89_8852be driver`.
2. That is enough for the stock speeds. For full speed, build and load our driver patch:

   ```sh
   sudo pacman -S --needed linux-headers       # or your kernel's -headers package
   scripts/rtw89-build.sh                      # once per kernel version
   scripts/rtw89-load.sh load                  # once per boot; Wi-Fi drops for a few seconds
   airdrop doctor | grep rtw89                 # "[ok ] ... AirDrop build loaded (awdl_ba)"
   ```

   `scripts/rtw89-load.sh install-boot` loads it at every boot (a systemd unit; `uninstall-boot` removes it).
   After a kernel update, run `scripts/rtw89-build.sh` again: the load script refuses a module built for
   another kernel and leaves the stock driver in place.
3. Send and receive as usual. Nothing on this card needs a flag.

`scripts/rtw89-load.sh revert` or a reboot brings the stock driver back. The modules on disk are never touched.

## What the helper does on this card

- **The iPhone becomes a station of the hidden access point** in every session. Without that, the chip sends
  frames for an unknown peer under the access point's own entry and releases them 17 at a time every ~140 ms:
  0.2 MB/s sending, 0.56 MB/s receiving, and the iPhone retries most of its frames to us.
- **HT rates** (MCS 15, 40 MHz) for single frames; the stock driver honours injected HT and VHT rates.
- **No active monitor mode** on this card. That is fine in borrow mode: the access point carries our address and
  the chip acknowledges for it. Shared mode (Wi-Fi kept connected) may not work well here.
- With our build: the `awdl_ba` parameter on for the session (restored afterwards), Block Ack asked for in AWDL's
  format, HT/VHT elements in the access point's beacon. The firmware then aggregates at up to 866.7 Mbit/s.

## Checking that it works

During or right after a send:

```sh
journalctl -k -b | grep awdl_ba          # "awdl_ba: <address> tid 0: aggregation on (window 64)"
grep "Block Ack" /run/airdrop-owl.log    # "asked ...", "... agreed; its frames are aggregated from here on"
grep "station:" /run/airdrop-helper.log  # tx bitrate 866.7 MBit/s VHT-MCS 9 80MHz short GI VHT-NSS 2
cat /sys/module/rtw89_core/parameters/awdl_ba   # N again after the session
```

For a slow run, take a trace and read it with `scripts/air-check.py` ([Faster on your card](Faster-on-your-card.md)).

## Known behaviour

- `rtw89_8852be ...: timed out to flush queues` in the kernel log when a session ends: frames left for an iPhone
  that already went off channel. The stock driver prints it too; it costs about 0.3 s of teardown.
- The Wi-Fi interface's `phy` number goes up each time the modules are reloaded (`phy0`, then `phy1`, ...).
  Anything you script against debugfs should look it up (`readlink /sys/class/net/wlan0/phy80211`).
- Power saving makes no difference to AirDrop on this card (tested with `rtw89_core disable_ps_mode=Y` and
  `iw ... power_save off`); neither do Bluetooth coexistence, mac80211's airtime limiter, or the chip's dynamic
  EDCCA. These were all ruled out while looking for the 0.2 MB/s cause.

## How it was found

`docs/FINDINGS.md` findings 14-16 and the "E-RTW89" entries in `docs/EXPERIMENTS.md`. The useful tools were an
air capture (bursts of exactly 17 acknowledged frames), the chip's own DMA indexes through rtw89 debugfs
(`read_reg`, showing every frame had been fetched) and one test with the iPhone as a station
(`scripts/rtw89-phase0.sh`).

## Other rtw89 chips

The RTL8852AE/CE/BTE, 8851BE and 8922AE use the same `rtw89_core`. The station path in the helper applies to the
whole family (`rtw89_*` drivers), untested. The build script builds the 8852BE stack only; other chips need their
module names added to `STACK` and the `off=` list in `scripts/rtw89-build.sh`. Reports welcome.
