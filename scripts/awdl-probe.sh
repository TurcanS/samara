#!/usr/bin/env bash
# Radio-level diagnosis: can this machine hear an iPhone doing AirDrop?
#   1. Bluetooth LE: does the iPhone send AirDrop advertisements?
#   2. Wi-Fi: in monitor mode on channels 6/44/149, do we capture frames at all,
#      and do we capture Apple AWDL action frames?
# Keep the iPhone's AirDrop share sheet OPEN for the whole run (~60 s).
# Wi-Fi is disconnected during the run and restored at the end (also on Ctrl-C).
#
#   scripts/awdl-probe.sh [iface]       -> writes probe/report.txt + probe/*.pcap
set -u
[[ $EUID -eq 0 ]] || exec sudo -- "$0" "$@"

IFACE=${1:-wlan0}
DUR=${DUR:-15}
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/probe"
OWNER=${SUDO_USER:-root}
rm -rf "$OUT" && mkdir -p "$OUT"
REPORT="$OUT/report.txt"
: >"$REPORT"
log() { echo "$*" | tee -a "$REPORT"; }

restore() {
  trap - EXIT INT TERM
  echo "Restoring Wi-Fi..."
  ip link set "$IFACE" down 2>/dev/null
  iw dev "$IFACE" set type managed 2>/dev/null
  ip link set "$IFACE" up 2>/dev/null
  nmcli device set "$IFACE" managed yes 2>/dev/null
  chown -R "$OWNER": "$OUT"
  echo "Report: $REPORT"
}
trap restore EXIT INT TERM

log "== system"
log "kernel: $(uname -r)"
log "driver: $(basename "$(readlink -f /sys/class/net/$IFACE/device/driver)" 2>/dev/null)"
dmesg 2>/dev/null | grep -iE "mt79|firmware" | tail -4 >>"$REPORT"

echo
echo ">>> On the iPhone: open a photo -> Share -> AirDrop, and KEEP THE SHEET OPEN until this finishes."
for i in 5 4 3 2 1; do printf "\rstarting in %s " "$i"; sleep 1; done; echo

log ""
log "== bluetooth (10 s scan)"
btmgmt power on >/dev/null 2>&1
timeout 12 btmon >"$OUT/btmon.txt" 2>&1 &
BTMON=$!
sleep 0.5
timeout 10 btmgmt find -l >/dev/null 2>&1
wait "$BTMON" 2>/dev/null
log "LE adverts seen:        $(grep -c 'LE Advertising Report' "$OUT/btmon.txt")"
log "Apple adverts:          $(grep -c 'Company: Apple' "$OUT/btmon.txt")"
log "Apple AirDrop (0x05):   $(grep -A3 'Company: Apple' "$OUT/btmon.txt" | grep -ciE 'Data: 05|AirDrop')"

log ""
log "== wi-fi monitor capture (${DUR}s per channel)"
nmcli device set "$IFACE" managed no 2>/dev/null
sleep 1
ip link set "$IFACE" down
iw dev "$IFACE" set type monitor || { log "cannot set monitor mode"; exit 1; }
ip link set "$IFACE" up

# Action frame (fc 0xd0), vendor-specific category 127, Apple OUI 00:17:f2 = AWDL.
AWDL='wlan[0] = 0xd0 and wlan[24] = 0x7f and wlan[25] = 0x00 and wlan[26] = 0x17 and wlan[27] = 0xf2'
for ch in 6 44 149; do
  if ! iw dev "$IFACE" set channel "$ch" 2>>"$REPORT"; then
    log "channel $ch: cannot tune"
    continue
  fi
  f="$OUT/ch$ch.pcap"
  timeout "$DUR" tcpdump -i "$IFACE" -nn -s 512 -w "$f" >/dev/null 2>&1
  total=$(tcpdump -r "$f" -nn 2>/dev/null | wc -l)
  beacons=$(tcpdump -r "$f" -nn 'type mgt subtype beacon' 2>/dev/null | wc -l)
  awdl=$(tcpdump -r "$f" -nn "$AWDL" 2>/dev/null | wc -l)
  srcs=$(tcpdump -r "$f" -nn -e "$AWDL" 2>/dev/null | grep -oE 'SA:[0-9a-f:]{17}' | sort -u | tr '\n' ' ')
  log "channel $ch: frames=$total beacons=$beacons awdl_action=$awdl ${srcs:+awdl_senders: $srcs}"
done
log ""
log "done"
