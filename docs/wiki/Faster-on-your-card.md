# Faster on your card

The same code ran at 0.2 MB/s on one card and 40 MB/s on another. Almost all of that difference was the Wi-Fi
driver. This page is the checklist that found the causes on the two tested cards, in the order that wastes the
least time. You need one iPhone set to **AirDrop → Everyone for 10 Minutes** (it expires; set it again between
runs), sudo, and `tcpdump`.

## 1. Check the basics: `airdrop doctor`

| Line | What to look for |
|---|---|
| regulatory domain / channel 149 | must "may transmit". Country `00` makes the kernel drop every frame on 149 silently: `sudo iw reg set <your country>` |
| monitor mode | required; without it AirDrop cannot work on this card |
| active monitor | nice to have. Borrow mode (the default) does not need it |
| `<driver> driver` | what the helper does for your driver; for an unknown driver, HT rates and single frames |

Then one plain run in each direction, to have numbers to compare against:

```sh
airdrop send -r "device name" BIGFILE      # prints MB/s at the end
airdrop receive                            # start it first, then share from the iPhone
```

Rough guide for a 2x2 card at 80 MHz: single frames top out around 5-6 MB/s sending; receive should reach
15-25 MB/s, because the iPhone aggregates on its own once we accept its Block Ack request. Anything far below that
has a cause worth finding.

## 2. Take a trace and let `air-check.py` read it

```sh
NAME=send DUR=60 scripts/airdrop-trace.sh send --mode borrow -r "device name" BIGFILE
python3 -I scripts/air-check.py probe/sessions/<time>-send
```

It prints how our frames were acknowledged (single ACKs or Block Acks, burst sizes, silences, slots used), how
often the iPhone had to retry its frames to us, Block Ack requests our driver sent, the TCP round trip, and OWL's
log, then names the likely cause. The trace folder holds addresses and device names: keep it to yourself and
share only the text output.

## 3. Symptoms and what they meant on the tested cards

| `air-check.py` shows | What it was | What fixed it |
|---|---|---|
| Nothing acknowledged at all | the driver sent injected unicast at a rate that does not exist on 5 GHz (stock MT7921: 1 Mbit/s CCK), or a No-IR channel | a driver patch that keeps the requested rate; `iw reg set` |
| Bursts of a fixed small size (17) with ~140 ms silences; iPhone retries > 50 %; TCP round trip 300 ms | the chip held frames for a peer it had no station entry for (stock rtw89) | the iPhone as a station of our access point: `AIRDROP_HELPER_ARGS=--tx-sta` (automatic on rtw89) |
| Block Ack requests "in mac80211's format" | the driver starts its own Block Ack session for every station; the iPhone never answers, and the queue stops up to 1 s each time | a driver patch that skips this for injected frames (`driver/rtw89-awdl.diff`) |
| Single frames only, ~5 MB/s, everything else clean | the ceiling without aggregation | a driver that turns the firmware's Block Ack on when the iPhone accepts OWL's request (`awdl_ba` in both patches) |
| Block Acks, but 10 MB/s instead of 30 | the firmware sent at 20 MHz: the access point's beacon had no HT/VHT elements | the helper adds them whenever the driver has `awdl_ba` |
| Long silences with no pattern, rate stepping down in owl.log | the iPhone is far away or rarely on our channel, or our own Wi-Fi scans move the radio | move closer; check `scan guard` lines in `/run/airdrop-helper.log` |
| Many "unable to inject" | the driver drains frames slower than OWL hands them over | look at the bursts for where they wait |

## 4. Test one cause at a time

The helper takes experiment flags through the environment, for one run, without changing anything else
(`docs/HELPER.md` lists them all):

```sh
AIRDROP_HELPER_ARGS="--tx-sta"            airdrop send ...   # the iPhone as a station, no Block Ack request
AIRDROP_HELPER_ARGS="--tx-rate 54"        airdrop send ...   # a fixed rate: 6..54, mcs0..mcs15[-40][-sgi]
AIRDROP_HELPER_ARGS="--no-ampdu"          airdrop send ...   # refuse the iPhone's Block Ack request
AIRDROP_HELPER_ARGS="--narrow"            airdrop send ...   # a 20 MHz access point instead of 80
AIRDROP_HELPER_ARGS="--no-scan-guard"     airdrop send ...   # let Wi-Fi scans happen (to see what they cost)
```

**Does the driver honour the injected rate?** Run `--tx-rate 54`, `--tx-rate mcs7` and the default, 40 s each,
and compare the `traffic:` lines in `/run/airdrop-owl.log` (frames per 5 s). If the count does not change with
the rate, the driver ignores it. If it changes but stays far below a few thousand frames per second, frames wait
somewhere: step 5.

Things that turned out **not** to matter on the Realtek, each tested with one switch (worth one run on a new card,
not more):

| Suspect | How to test it |
|---|---|
| Wi-Fi power saving | `sudo iw dev wlan0 set power_save off`; on rtw89 `echo Y \| sudo tee /sys/module/rtw89_core/parameters/disable_ps_mode` |
| Bluetooth sharing the antenna | the driver's coexistence state, e.g. rtw89 `.../rtw89/btc_info` in debugfs (on 5 GHz Wi-Fi owned the antenna) |
| mac80211's airtime limiter | `echo 0 \| sudo tee /sys/kernel/debug/ieee80211/<phy>/aql_enable` for one run (restore `1` afterwards) |
| the chip's busy-channel check | rtw89: `echo 0x41 \| sudo tee .../rtw89/disable_dm` for one run (restore `0`) |

## 5. Where do frames wait?

When frames leave in bursts, find the stage that holds them:

- **mac80211:** `/sys/kernel/debug/ieee80211/<phy>/aqm` during a send. `fq_backlog` near 0 means not here.
- **The DMA ring:** the driver's own registers. On rtw89, `scripts/rtw89-phase0.sh` samples the TX ring indexes
  every 50 ms; host index equal to hardware index means the chip has fetched every frame and holds them inside.
- **On air:** the air capture. Frames the iPhone acknowledges within milliseconds, then silence, means the radio
  was idle, not busy retrying.

## 6. What a driver needs for full speed

Everything above comes down to four properties. The two patches in `driver/` add the missing ones; they are
written to be read, and a third card would follow the same shape.

1. **Injected unicast at the radiotap rate** (legacy and HT at least). Stock rtw89 does this; stock mt7921e does
   not.
2. **Frames to a registered station keep moving.** The helper registers the iPhone as a station of the hidden
   access point; the chip must then use that station's entry for injected frames.
3. **No Block Ack negotiation of its own for injected frames.** The iPhone answers only AWDL's frame format,
   which OWL sends.
4. **Firmware aggregation switched on from outside**: when the iPhone's ADDBA response (BSSID
   `00:25:00:ff:94:73`, status 0) arrives from a station, enable the firmware's Block Ack originator for that
   station and TID, as the driver's own `ampdu_action(TX_OPERATIONAL)` would; a DELBA switches it off. Frames
   OWL then sends without a radiotap rate go out with the firmware's rate control.

## 7. Reporting a card

Open an issue with: the `lspci -nn` (or `lsusb`) line, `uname -r`, `airdrop doctor`, the speed lines of one send
and one receive, and the text of `scripts/air-check.py` for a slow run, addresses replaced. Never attach the trace
folder itself (`CONTRIBUTING.md`).
