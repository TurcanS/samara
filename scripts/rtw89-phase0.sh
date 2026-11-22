#!/bin/sh
# What holds an rtw89 card back (2026-11-22, EXPERIMENTS "E-RTW89 Phase 0"): three 40-second sends to an
# iPhone, no kernel build. Run it with the stock driver and an older helper to see the original hold.
#   A  as now
#   B  --tx-sta: the iPhone becomes a station of our access point (OWL -g), frames keep OWL's rate
#   C  --no-ampdu: we refuse the iPhone's Block Ack request (it sends single frames)
# Each run records, in probe/sessions/<time>-p0-<run>/:
#   air.pcap   the radio (awdlmon), for the burst/ACK-gap analysis
#   txbd.txt   the chip's TX queue indexes of ACH0-3 every 50 ms (host idx low 16 bits, hw idx high 16)
#   owl.log, helper.log
# Accept every request on the iPhone (AirDrop: Everyone for 10 Minutes, screen on). DEVICE is
# (part of) its name, as for `airdrop send -r`.
#
#   scripts/rtw89-phase0.sh DEVICE FILE [A B C]
set -u
DEVICE=${1:?usage: $0 DEVICE FILE [A B C]}
FILE=${2:?usage: $0 DEVICE FILE [A B C]}; shift 2
RUNS=${*:-A B C}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PHY=$(basename "$(readlink -f /sys/class/net/${IFACE:-wlan0}/phy80211)")
RR=/sys/kernel/debug/ieee80211/$PHY/rtw89/read_reg
sudo -v || exit 1

run() {
    name=$1; shift
    out="$ROOT/probe/sessions/$(date +%Y%m%d-%H%M%S)-p0-$name"
    mkdir -p "$out"
    echo "=== $name: ${*:-as now}  ->  $out"
    AIRDROP_HELPER_ARGS="$*" timeout -s INT 40 airdrop send -r "$DEVICE" "$FILE" &
    pid=$!
    # Start the capture once the monitor interface exists.
    for _ in $(seq 100); do [ -e /sys/class/net/awdlmon ] && break; sleep 0.1; done
    sudo tcpdump -i awdlmon -nn -s 128 -U -w "$out/air.pcap" >/dev/null 2>&1 &
    sudo sh -c "echo '1058 10' > $RR; while kill -0 $pid 2>/dev/null; do
        printf '%s ' \"\$(date +%s.%N)\"; cat $RR | tail -1; sleep 0.05; done" > "$out/txbd.txt" 2>/dev/null &
    wait $pid
    sleep 3
    sudo pkill -f "tcpdump -i awdlmon -nn -s 128 -U -w $out/air.pcap" 2>/dev/null
    sudo cp /run/airdrop-owl.log "$out/owl.log"; sudo cp /run/airdrop-helper.log "$out/helper.log"
    sudo chown -R "$(id -u):$(id -g)" "$out"
    grep -E "traffic" "$out/owl.log" | tail -4
    sleep 8
}

for r in $RUNS; do
    case $r in
        A) run A ;;
        B) run B --tx-sta ;;
        C) run C --no-ampdu ;;
    esac
done
echo "done: probe/sessions/*-p0-*"
