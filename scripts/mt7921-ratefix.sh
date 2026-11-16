#!/usr/bin/env bash
# Swap the MT7921 Wi-Fi driver module for the patched build, temporarily.
#
#   scripts/mt7921-ratefix.sh load     # use vendor/mt76-driver/mt7921e-awdl-$(uname -r).ko
#   scripts/mt7921-ratefix.sh revert   # back to the stock module (a reboot does this too)
#   scripts/mt7921-ratefix.sh status
#   scripts/mt7921-ratefix.sh install-boot    # load it at every boot (systemd unit)
#   scripts/mt7921-ratefix.sh uninstall-boot
#
# Build the module first with scripts/mt7921-build.sh (driver/README.md says what the patch
# changes). It touches injected frames and AWDL peers only; normal Wi-Fi traffic is unchanged.
# Nothing is installed: the stock module on disk is untouched, so a reboot reverts.
# Wi-Fi drops for a few seconds during the swap and NetworkManager reconnects.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRV="$ROOT/vendor/mt76-driver"
# A module carries a vermagic and only loads into the kernel it was built for.
KO="${KO:-$DRV/mt7921e-awdl-$(uname -r).ko}"
# The root helper cannot see which build is loaded, so `load` leaves it a note: this file holds
# the srcversion of a module that sends injected frames at HT rates. The helper compares it with
# the loaded module and only then asks OWL for HT rates. /run is cleared at reboot, like the module.
HT_MARK=/run/airdrop-mt7921-ht
IFACE=${IFACE:-wlan0}

check_vermagic() {
  local want got
  want="$(uname -r)"; got="$(modinfo -F vermagic "$KO" 2>/dev/null | awk '{print $1}')"
  if [ -n "$got" ] && [ "$got" != "$want" ]; then
    echo "ERROR: $KO is built for $got, kernel is $want. Rebuild: scripts/mt7921-build.sh" >&2
    return 1
  fi
}

loaded_srcversion() { cat /sys/module/mt7921e/srcversion 2>/dev/null; }
fix_srcversion() { modinfo -F srcversion "$KO" 2>/dev/null; }

mark_ht() {
  local sv; sv="$(loaded_srcversion)"
  if [ -n "$sv" ] && [ "$sv" = "$(fix_srcversion)" ]; then
    [ "$(cat "$HT_MARK" 2>/dev/null)" = "$sv" ] || sudo sh -c "umask 022; echo '$sv' > '$HT_MARK'"
  elif [ -e "$HT_MARK" ]; then
    sudo rm -f "$HT_MARK"
  fi
}

wait_wifi() {
  for _ in $(seq 60); do
    state=$(nmcli -t -f DEVICE,STATE device 2>/dev/null | awk -F: -v d="$IFACE" '$1==d{print $2}')
    if [ "$state" = "connected" ]; then echo "Wi-Fi: connected"; return 0; fi
    sleep 0.5
  done
  echo "Wi-Fi: not connected yet (state: ${state:-missing}); NetworkManager may still be working on it"
  return 1
}

status() {
  if [ -z "$(loaded_srcversion)" ]; then
    echo "mt7921e: not loaded"
  elif [ -e /sys/module/mt7921e/parameters/awdl_ba ]; then
    echo "mt7921e: PATCHED build loaded: HT rates and Block Ack for AWDL peers (awdl_ba=$(cat /sys/module/mt7921e/parameters/awdl_ba))"
  else
    echo "mt7921e: stock module loaded"
  fi
}

busy() {
  [ -e /run/airdrop-helper.state ] || return 1
  echo "AirDrop is running; stop it first (or run: airdrop recover)"
}

case "${1:-status}" in
  load)
    [ -f "$KO" ] || { echo "missing $KO (build it first: scripts/mt7921-build.sh)"; exit 1; }
    check_vermagic || exit 1
    busy && exit 1
    if [ "$(loaded_srcversion)" = "$(fix_srcversion)" ]; then mark_ht; status; exit 0; fi
    echo "Swapping in the patched mt7921e (Wi-Fi drops for a few seconds)..."
    sudo sh -c "
      rmmod mt7921e || exit 1
      modprobe mt7921-common
      if ! insmod '$KO'; then
        echo 'loading the patched module FAILED - restoring the stock driver' >&2
        modprobe mt7921e
        exit 2
      fi
    "
    rc=$?
    mark_ht
    status
    wait_wifi
    exit $rc
    ;;
  revert)
    busy && exit 1
    echo "Restoring the stock mt7921e (Wi-Fi drops for a few seconds)..."
    sudo sh -c "rmmod mt7921e 2>/dev/null; rmmod mt7921_common 2>/dev/null; modprobe mt7921e"
    mark_ht
    status
    wait_wifi
    ;;
  install-boot)
    # Boot-time load. Fail-safe: a module built for another kernel is refused before anything
    # is touched, and a failed insmod restores the stock driver (see `load`).
    sudo sh -c "cat > /etc/systemd/system/airdrop-mt7921-ratefix.service" <<UNIT
[Unit]
Description=Load the patched MT7921 Wi-Fi driver for AirDrop
Wants=sys-subsystem-net-devices-$IFACE.device
After=sys-subsystem-net-devices-$IFACE.device NetworkManager.service

[Service]
Type=oneshot
Environment=IFACE=$IFACE
ExecStart=$ROOT/scripts/mt7921-ratefix.sh load
SuccessExitCodes=0 1 2

[Install]
WantedBy=multi-user.target
UNIT
    sudo systemctl daemon-reload && sudo systemctl enable airdrop-mt7921-ratefix.service
    ;;
  uninstall-boot)
    sudo systemctl disable airdrop-mt7921-ratefix.service
    sudo rm -f /etc/systemd/system/airdrop-mt7921-ratefix.service
    sudo systemctl daemon-reload
    ;;
  status)
    status
    ;;
  *)
    echo "usage: $0 load|revert|status|install-boot|uninstall-boot"; exit 2
    ;;
esac
