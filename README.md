# Samara

**AirDrop for Linux, from the terminal, on the one Wi-Fi card your laptop already has.**

Send files, folders and links to iPhones, iPads and Macs, and receive from them. Works with Apple devices set
to **AirDrop → Everyone for 10 Minutes**.

```
airdrop find                          # list nearby receivers
airdrop send photo.jpg "My Folder"    # pick a device from the list
airdrop send -r "anna's iphone" movie.mp4   # send to the first device whose name contains this
airdrop send --url https://example.com
airdrop receive                       # be visible; files land in ~/Downloads/AirDrop
airdrop listen                        # background: Wi-Fi stays up until an iPhone nearby opens AirDrop
airdrop doctor                        # check hardware, OWL, polkit, Bluetooth, driver
airdrop recover                       # undo a crashed session, give Wi-Fi back
```

| Direction | Speed | Run |
|---|---|---|
| iPhone → Linux | **21 MB/s** | 150.9 MB, 25 items, in 7.2 s |
| Linux → iPhone | **30.6 MB/s** | 417 MB in 13.6 s |

Measured with a MediaTek MT7921 and an iPhone 17 on iOS 27.2. How those numbers were reached, including the
wrong turns, is in [`docs/FINDINGS.md`](docs/FINDINGS.md).

A samara is the winged seed of a maple: a small thing that travels by air.

> AirDrop is a trademark of Apple Inc. This project is not affiliated with or endorsed by Apple. It is
> interoperability work: it implements the open "Everyone" mode and nothing else.

## Status

Tested on **one laptop and one phone**: Arch Linux, kernel 7.2.5, MediaTek MT7921 (`14c3:7961`), NetworkManager,
BlueZ; iPhone 17, iOS 27.2. A few other iPhones nearby have sent and received along the way. Other cards, other
distributions and Macs are untested. Reports are welcome, see [`CONTRIBUTING.md`](CONTRIBUTING.md).

Known open issue: the iPhone lists us reliably only when `airdrop receive` is already running before its share
sheet opens (finding 10). **Start `receive` first, then open the share sheet.** `airdrop listen` reacts about
2.5 s after the sheet opens, which is at the edge.

## How it works

| Layer | What runs |
|---|---|
| Wi-Fi link | [OWL](https://github.com/seemoo-lab/owl) (with [our patch](packaging/owl-airdrop.patch)) implements Apple's AWDL and creates `awdl0` |
| Wake-up | BLE AirDrop advertisement (BlueZ over D-Bus) so iPhones switch AWDL on when you send |
| Discovery | mDNS `_airdrop._tcp` on `awdl0` (IPv6 link-local) |
| Transfer | HTTPS `/Discover` → `/Ask` → `/Upload`; payload is a cpio archive in Apple's `dvzip` container (iOS 26+), gzip'd cpio also accepted |

Root is needed only for the radio and BLE. `airdrop` runs as your user and starts a small helper with
**pkexec**. The helper lives exactly as long as the client: when `airdrop` exits, crashes or is killed, the
helper's stdin closes and it undoes everything. Teardown is fail-safe (every step runs even if another fails or
the terminal is gone), and what was changed is recorded in `/run/airdrop-helper.state`, so `airdrop recover` can
undo it even if the helper itself died. The contract is in [`docs/HELPER.md`](docs/HELPER.md).

### Wi-Fi modes (`--mode`, config `mode`)

AirDrop runs on channel 149 (5 GHz), which is rarely the channel your router uses.

- `auto` (default): `shared` when your Wi-Fi already is on the AirDrop channel, else `borrow`.
- `borrow`: **Wi-Fi is paused for the session** (about 1.5 s to start) and reconnects to the same network when
  AirDrop stops. A hidden access point of our own holds the radio on channel 149 at 80 MHz, and OWL speaks AWDL
  through a pair of monitor interfaces beside it. This is the mode that is fast and that is tested.
  `--channel 6` is for an iPhone whose Wi-Fi is on 2.4 GHz or off.
- `shared`: **your Wi-Fi stays connected** and AirDrop uses its channel through the monitor pair. Only useful
  when that channel is 149 (6 or 44 also work for sending); elsewhere the iPhone rarely visits and does not
  find us.

There is no mode that takes Wi-Fi offline for longer than one session, and teardown always gives it back.
`airdrop doctor` tells you which mode your current connection gets.

## Install

You need a Wi-Fi card with monitor mode and frame injection, Bluetooth LE, NetworkManager, polkit, and a
regulatory domain in which channel 149 may transmit.

### From source

```sh
sudo pacman -S rust openssl iw bluez libpcap libev libnl cmake git   # Arch; other distributions: the same libraries
scripts/build-owl.sh --install        # OWL at a pinned commit + our patch -> /usr/local/lib/airdrop/owl (root-owned)
cargo build --release --locked        # target/release/airdrop and target/release/airdrop-helper
export AIRDROP_HELPER=$PWD/target/release/airdrop-helper   # or install it, see doctor
target/release/airdrop doctor         # [ERR] lines say what is still missing
```

Without the polkit action installed, pkexec shows its generic admin prompt every time. `doctor` prints the
`pkexec install` lines that put the helper and OWL where the polkit action expects them (`/usr/lib/airdrop/`).

### MediaTek MT7921: the driver patch

The stock `mt7921e` sends a frame injected to one device at 1 Mbit/s, a rate that does not exist on 5 GHz, so it
is never transmitted. One patch fixes that and adds aggregated sending; see [`driver/README.md`](driver/README.md).

```sh
scripts/mt7921-build.sh               # once per kernel version; downloads about 300 MB of kernel source
scripts/mt7921-ratefix.sh load        # once per boot; Wi-Fi blips for ~3 s. `revert` or a reboot undoes it
```

Nothing is installed: the stock module on disk is untouched.

### Arch package

```sh
cd packaging && makepkg -si
```

Builds OWL and installs `/usr/bin/airdrop`, `/usr/lib/airdrop/{owl,airdrop-helper}`, the polkit action and the
(disabled) user service. The driver patch stays a per-boot step from the source tree.

### If channel 149 may not transmit

`airdrop doctor` says so. The usual cause is an unset regulatory domain (`iw reg get` shows `country 00`), in
which the kernel silently drops every frame we send. Set your own country: `sudo iw reg set <country code>`.

## Receiving

Files go to `~/Downloads/AirDrop`. Existing files are never overwritten: a clash becomes `name (1).ext`.

`receive` and `listen` accept everything unless you pass `--ask`, which asks `[y]es / [n]o / [a]lways` first.
Answering `a` adds the sender to `known_senders` in `~/.config/airdrop/config.toml`, and later requests from that
name are accepted without asking. Sender names are self-reported, so this is a convenience, not security. An
unanswered question declines after 60 s (`--ask-timeout SECONDS`), and the sender is told at once. Links are
printed and opened only when you explicitly answer `y`, never on auto-accept, and only `http`/`https` ones.

Upload archives are checked as they stream in: absolute paths, `..` and symlinks are refused, and so is any item
the sender did not name in its request, so what is saved is what you were asked about. An upload that is not an
AirDrop archive is refused unread. A rejected or interrupted transfer is removed again.

### In the background (`airdrop listen`)

Leave it running in a terminal. Wi-Fi stays connected; only Bluetooth is watched. When an iPhone nearby opens
Share → AirDrop, Wi-Fi is paused, the laptop appears in its list, the file is saved and Wi-Fi reconnects.

- **It reacts to any iPhone nearby** by default. Limit it to your own phone with its contact hashes, which
  `airdrop listen --dry-run` prints: `airdrop listen --hash 1a2b --hash 3c4d`.
- Every trigger starts the root helper through pkexec, so expect a password prompt unless the optional polkit
  rule in `packaging/50-airdrop-background.rules.example` is installed.
- `airdrop listen --dry-run` only reports what it hears and never touches Wi-Fi.

`packaging/airdrop-receive.service` runs `airdrop listen --ask --prompter notify` as a user service: every
transfer is asked about through desktop-notification buttons (any notification server with action buttons; no
answer declines). A user service cannot show a password prompt, so it needs that polkit rule.

## Configuration

```
airdrop config show
airdrop config set name "Anna's Laptop"
airdrop config set wifi_iface wlan1
airdrop config set channel auto          # auto, 6, 36, 44 or 149
airdrop config known add "Anna's iPhone"
```

## Measure it yourself

```sh
NAME=rx   DUR=120 scripts/airdrop-trace.sh --mode borrow
NAME=send DUR=120 scripts/airdrop-trace.sh send --mode borrow -r "device name" FILE
python3 -I scripts/analyze-trace.py probe/sessions/<dir>
```

Each run writes `probe/sessions/<time>-NAME/` (logs, packet captures, protocol dumps) and needs sudo for the
capture. Those folders contain addresses, device names and identity records; `.gitignore` keeps them out of the
repository, and they should stay out of issue reports too.

| Document | What it is |
|---|---|
| [`docs/FINDINGS.md`](docs/FINDINGS.md) | the headline results and how to repeat each |
| [`docs/PROTOCOL.md`](docs/PROTOCOL.md) | AirDrop as iOS 27.2 runs it, layer by layer, every row tagged measured / static / hypothesis |
| [`docs/EXPERIMENTS.md`](docs/EXPERIMENTS.md) | the lab log: every run with its prediction, result and corrections |
| [`docs/HELPER.md`](docs/HELPER.md) | the root helper's contract and flag history |
| [`docs/RESEARCH.md`](docs/RESEARCH.md) | background and what other projects found |
| [`driver/README.md`](driver/README.md) | the MT7921 driver patch |

## Tests

```sh
cargo build --release && python -m pytest -q      # needs python-pytest
```

The suite drives the real CLI like a user would, over loopback with AWDL skipped, so it needs no radio.
`AIRDROP_CLI=/usr/bin/airdrop` tests an installed build instead; `AIRDROP_CLI_SENDER=...` makes two builds send
to each other. It covers sending files, folders and links; decline, always and auto-accept; an unanswered
prompt; hostile senders (path traversal, unannounced files, non-AirDrop uploads); iPhone-shaped uploads (chunked
requests, `TransferID`, dvzip with mixed blocks, staging folders, the withheld final chunk, several photos at
once); stalls, cut-off transfers and Ctrl-C.

## Limits

- Only "Everyone" mode. "Contacts Only" needs Apple-signed sender records and is out of scope.
- TLS peers are not verified. This is inherent to "Everyone" mode, where devices use self-signed certificates.
- Single files above 8 GiB are not supported (cpio odc format limit).
- One radio is shared with the iPhone's own schedule: it is on our channel for about two thirds of the time while
  a transfer runs, which bounds both directions.
- `-r` matches part of a device name. In a room full of phones, give enough of the name to be sure whose it is:
  the first match gets the request.
- A transfer with no progress for 60 s is stopped on either side; a partly received file is removed.
  `AIRDROP_STALL_TIMEOUT=seconds` changes that.
- "N Apple devices in radio range but did not answer" when sending means the iPhone hears us but its AirDrop is
  not receiving: set **Everyone for 10 Minutes** again (it expires).
- If devices don't show up, check `airdrop -v find`, then `/run/airdrop-owl.log` and `/run/airdrop-helper.log`.

## Credits

- [OWL](https://github.com/seemoo-lab/owl) (SEEMOO Lab, TU Darmstadt): the AWDL implementation this runs on, and
  the Open Wireless Link research behind it.
- [owly](https://github.com/t4t5/omdrop-owl): the independent spec whose rule IDs `docs/PROTOCOL.md` refers to.
- [Windfall](https://github.com/UvejsGj/Windfall): showed that current iOS accepts non-Apple peers.
- [airdrop-mt7921](https://github.com/jedbillyb/airdrop-mt7921): findings on this chip and on iOS 26 uploads.
- [shaecodes-wilde/mt76](https://github.com/shaecodes-wilde/mt76): the injected-rate fix the driver patch starts from.
- [OpenDrop](https://github.com/seemoo-lab/opendrop) and the PrivateDrop paper (USENIX Security '21).

## License

GPL-3.0-or-later, see [`LICENSE`](LICENSE). The driver patch in `driver/` is GPL-2.0, like the kernel code it
changes; see [`NOTICE`](NOTICE).
