#!/usr/bin/env bash
# Full trace of one `airdrop receive` session, for diagnosis and reverse engineering.
# Each run gets its own folder, probe/sessions/<time>[-NAME]/ (never overwritten):
#   receive.log   client output (-v)
#   owl.log       OWL log
#   awdl0.pcap    IP traffic on awdl0 (mDNS, TLS)  -> scripts/tls-hello.py
#   air.pcap      raw 802.11 on the monitor interface -> scripts/awdl-decode.py
#   tx.pcap       the frames OWL sends (the monitor does not record our own)
#   ble.btsnoop   BLE (btmon) -> btmon -r ble.btsnoop > ble.txt; scripts/ble-airdrop.py ble.txt
#   dump/         every request as JSON (airdrop receive --dump)
#   iface.txt     awdl0 link / address / multicast state
#   iw-event.log  Wi-Fi events with timestamps (scans take the radio off the AWDL channel)
# Run as your user (not with sudo). Wi-Fi follows the mode: paused in borrow mode (the default
# off channel 149) and reconnected at the end; measure a session with scripts/analyze-trace.py.
# Extra arguments go to airdrop, e.g. a second channel:
#   NAME=link-share DUR=60 scripts/airdrop-trace.sh
#   NAME=go DUR=120 scripts/airdrop-trace.sh --go 149
#   NAME=send1 DUR=120 scripts/airdrop-trace.sh send --go 149 FILE   (iPhone as receiver)
#   NAME=find1 DUR=60  scripts/airdrop-trace.sh find --go 149        (dump the iPhone's TXT + /Discover reply)
# Open the iPhone's AirDrop sheet when told. Ctrl-C ends a session early (cleanly).
set -u
# First argument "send" or "find" runs that command instead of receive.
if [[ "${1:-}" == send || "${1:-}" == find ]]; then
  SUB=$1; shift
else
  SUB=receive
fi
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-$ROOT/probe/sessions/$(date +%Y%m%d-%H%M%S)${NAME:+-$NAME}}"
DUR=${DUR:-45}
mkdir -p "$OUT"
AIRDROP=${AIRDROP:-$(command -v airdrop || echo "$ROOT/target/release/airdrop")}  # AIRDROP=target/release/airdrop: the build in this tree
# Every background loop below runs only while this file exists. Without it, a
# session that ended early left its watcher waiting for awdl0, and the *next*
# session's capture landed in the old folder (seen 2026-11-03).
FLAG="$OUT/.running"
touch "$FLAG"

echo "sudo is needed for packet capture:"
sudo -v || exit 1
# Keep the sudo timestamp fresh for the end-of-session steps (pkill, chown).
( while [[ -e "$FLAG" ]]; do sudo -n -v 2>/dev/null; sleep 30; done ) &
sudo -n btmon -w "$OUT/ble.btsnoop" >/dev/null 2>&1 &
iw event -t >"$OUT/iw-event.log" 2>/dev/null &
IWEVENT=$!
# btmon only sees adverts while an LE scan runs; keep duplicates so rotation shows up.
# bluetoothctl must stay alive (BlueZ stops discovery when its client leaves),
# so its stdin is kept open until the session ends.
{ printf 'menu scan\nduplicate-data on\nback\nscan le\n'
  while [[ -e "$FLAG" ]]; do sleep 1; done; } |
  bluetoothctl >/dev/null 2>&1 &

(
  for _ in $(seq 480); do
    [[ -e "$FLAG" ]] || exit 0
    [[ -e /sys/class/net/awdl0 ]] && break
    sleep 0.5
  done
  [[ -e "$FLAG" && -e /sys/class/net/awdl0 ]] || exit 0
  sleep 1.5
  {
    ip -d link show awdl0; echo; ip -6 addr show dev awdl0; echo
    ip -6 maddr show dev awdl0; echo; ip -6 route show table all dev awdl0
    echo; ip -d link show awdlmon 2>/dev/null; echo; ip -d link show awdlgo 2>/dev/null
  } >"$OUT/iface.txt" 2>&1
  sudo -n tcpdump -i awdl0 -nn -U -w "$OUT/awdl0.pcap" >/dev/null 2>&1 &
  sudo -n tcpdump -i awdlmon -nn -s 400 -U -w "$OUT/air.pcap" >/dev/null 2>&1 &
  # What we transmit: the monitor above does not record our own frames. awdlact is the
  # interface OWL injects on, so this has every frame it hands to the driver, with its time.
  sudo -n tcpdump -i awdlact -nn -s 400 -U -Q out -w "$OUT/tx.pcap" >/dev/null 2>&1 &
  # The clock starts once AWDL is up (not while you type your password).
  for _ in $(seq "$DUR"); do [[ -e "$FLAG" ]] || exit 0; sleep 1; done
  pkill -TERM -u "$(id -u)" -f "airdrop -v .*--dump $OUT/dump"
) &
WATCH=$!

echo
if [[ $SUB == receive ]]; then
  echo ">>> When you see \"Visible as ...\", open Share -> AirDrop on the iPhone and keep it open."
  ARGS=(receive --accept-all --dump "$OUT/dump" "$@")
else
  echo ">>> iPhone: AirDrop 'Everyone for 10 Minutes', screen on and unlocked."
  ARGS=("$SUB" --dump "$OUT/dump" "$@")
fi
echo ">>> The trace stops by itself ${DUR}s after AWDL is up."
echo
PYTHONUNBUFFERED=1 "$AIRDROP" -v "${ARGS[@]}" 2>&1 |
  (trap '' INT; exec tee "$OUT/$SUB.log")   # Ctrl-C stops airdrop, not the log

rm -f "$FLAG"   # ends every background loop above
sleep 3   # helper teardown; tcpdump exits when the interfaces disappear
sudo -n pkill -f "tcpdump -i awdl0 -nn -U -w $OUT/" 2>/dev/null
sudo -n pkill -f "tcpdump -i awdlmon -nn -s 400 -U -w $OUT/" 2>/dev/null
sudo -n pkill -f "tcpdump -i awdlact -nn -s 400 -U -Q out -w $OUT/" 2>/dev/null
sudo -n pkill -f "btmon -w $OUT/ble.btsnoop" 2>/dev/null
kill "$IWEVENT" 2>/dev/null
wait "$WATCH" 2>/dev/null
cat /run/airdrop-owl.log >"$OUT/owl.log" 2>/dev/null   # world-readable; no sudo needed
sudo -n chown -R "$(id -u):$(id -g)" "$OUT" 2>/dev/null ||
  echo "(sudo expired: some capture files stay root-owned; they are still readable)"
echo
echo "Trace saved in $OUT"
