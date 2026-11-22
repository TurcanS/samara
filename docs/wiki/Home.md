# Samara wiki

AirDrop for Linux on the one Wi-Fi card a laptop already has. How well it works depends almost entirely on that
card's driver: the same code ran at 0.2 MB/s on one card and 40 MB/s on another until the driver was understood.

## Cards

| Card | Driver | Status | iPhone → Linux | Linux → iPhone | Page |
|---|---|---|---|---|---|
| Realtek RTL8852BE | `rtw89_8852be` + our patch | works, fastest so far | 25.2 MB/s | 40.5 MB/s | [Realtek RTL8852BE](Realtek-RTL8852BE.md) |
| Realtek RTL8852BE | stock `rtw89_8852be` | works | 20.5 MB/s | 5.5 MB/s | [Realtek RTL8852BE](Realtek-RTL8852BE.md) |
| MediaTek MT7921 | `mt7921e` + our patch | works | 21 MB/s | 30.6 MB/s | [MediaTek MT7921](MediaTek-MT7921.md) |
| MediaTek MT7921 | stock `mt7921e` | does not work on 5 GHz | – | – | [MediaTek MT7921](MediaTek-MT7921.md) |

All measured with an iPhone 17 on iOS 27.2, 417 MB video unless noted, in borrow mode on channel 149 at 80 MHz.

Any other card: start with `airdrop doctor`, then [Faster on your card](Faster-on-your-card.md). It walks
through finding what holds a card back, using what was found on these two as the examples.

## Pages

- [Realtek RTL8852BE](Realtek-RTL8852BE.md): setup, the stock driver and our build, how to check it is working.
- [MediaTek MT7921](MediaTek-MT7921.md): setup, why the stock driver cannot work, the patched build.
- [Faster on your card](Faster-on-your-card.md): measure a transfer, read the symptoms, test the usual causes,
  and what a driver needs for full speed.

## Elsewhere in the repository

- [`README.md`](../../README.md): install, commands, modes.
- [`driver/README.md`](../../driver/README.md): the two driver patches.
- [`docs/FINDINGS.md`](../FINDINGS.md): the headline results and how to repeat each.
- [`docs/EXPERIMENTS.md`](../EXPERIMENTS.md): the lab log, every run with its prediction and result.
- [`CONTRIBUTING.md`](../../CONTRIBUTING.md): what to put in a hardware report, and what never to attach.
