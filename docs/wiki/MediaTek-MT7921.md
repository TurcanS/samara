# MediaTek MT7921 (`mt7921e`)

| | |
|---|---|
| Chip | MediaTek MT7921, Wi-Fi 6, 2x2, PCIe (`14c3:7961`); the MT7922 uses the same driver (untested) |
| Driver | `mt7921e` (with `mt7921-common`, `mt76`) |
| Tested | Arch Linux, kernel 7.2.5, NetworkManager; iPhone 17, iOS 27.2 |
| Stock driver | **does not work on 5 GHz** |
| With our patch | **receive 21 MB/s, send 30.6 MB/s** (VHT MCS 9, 80 MHz, two streams) |

## Why the stock driver cannot work

The stock `mt7921e` clears the rate of a frame injected to one device and the firmware falls back to 1 Mbit/s CCK.
That rate does not exist on 5 GHz, so the frame is never transmitted: nothing we send to the iPhone, not even
TCP acknowledgements, reaches it. Our patch keeps the rate the sender asked for, adds HT rates, and adds Block
Ack for AWDL peers (`driver/README.md`).

## Setup

```sh
sudo pacman -S --needed linux-headers       # or your kernel's -headers package (clang kernels: also clang, llvm, lld)
scripts/mt7921-build.sh                     # once per kernel version; downloads about 300 MB of kernel source
scripts/mt7921-ratefix.sh load              # once per boot; Wi-Fi drops for about 3 s
airdrop doctor | grep mt7921e               # "fast build with aggregated sending loaded"
```

`scripts/mt7921-ratefix.sh install-boot` loads it at every boot; `revert` or a reboot brings the stock driver
back. After a kernel update, build again.

## What the helper does on this card

- Switches the mt76 power saving (`runtime-pm`, `deep-sleep` in debugfs) off for the session: with it on, monitor
  reception silently stops.
- Borrow mode: a hidden access point holds the radio on channel 149 at 80 MHz. The card's firmware does not apply
  a monitor interface's channel on its own, so the access point is what keeps the radio there.
- Uses the active monitor mode the card has, so the chip acknowledges the iPhone's frames.
- Sending: the iPhone becomes a station of the access point, OWL asks it for Block Ack in AWDL's format, the
  patched driver switches the firmware's aggregation on (`awdl_ba`), and the beacon carries HT/VHT elements (without
  them the firmware sends at 20 MHz: 10 MB/s instead of 30).

## Checking that it works

```sh
journalctl -k -b | grep awdl_ba          # "awdl_ba: <address> tid 0: aggregation on"
grep "station:" /run/airdrop-helper.log  # tx bitrate 866.7 MBit/s VHT-MCS 9 80MHz short GI VHT-NSS 2
scripts/mt7921-ratefix.sh status
```

## Known behaviour

- Speeds by driver build: stock does not work; rate-fixed only about 1 MB/s; HT build about 5 MB/s (single
  frames); the aggregating build 30.6 MB/s. `airdrop doctor` names the loaded build.
- A regulatory domain of `00` makes the kernel drop every frame on channel 149 without an error. `airdrop doctor`
  says so; `sudo iw reg set <country>` fixes it.
- Keep the AWDL address random per session (the default): on one fixed address the iPhone stopped offering Block
  Ack (findings 11).

## How it was found

`docs/FINDINGS.md` findings 1-8 and the "E-TXAGG" entries in `docs/EXPERIMENTS.md`.
