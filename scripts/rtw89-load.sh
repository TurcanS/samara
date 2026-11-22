#!/usr/bin/env bash
# Swap the Realtek rtw89 8852BE driver stack for our build, temporarily.
#
#   scripts/rtw89-load.sh load [awdl|stock]  # our patched build (default), or the stock-source rebuild
#   scripts/rtw89-load.sh revert             # back to the distribution's modules (a reboot does this too)
#   scripts/rtw89-load.sh status
#   scripts/rtw89-load.sh install-boot       # load it at every boot (systemd unit); uninstall-boot
#
# Built by scripts/rtw89-build.sh into vendor/rtw89-driver/<module>-<variant>-$(uname -r).ko. The five
# modules are always swapped together (one source tree; no MODVERSIONS to catch a mismatch). Nothing is
# installed: the modules on disk are untouched, so a reboot reverts. Wi-Fi drops for a few seconds
# during the swap and NetworkManager reconnects.
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRV="$ROOT/vendor/rtw89-driver"
IFACE="${IFACE:-wlan0}"
KREL="$(uname -r)"
STACK=(rtw89_core rtw89_pci rtw89_8852b_common rtw89_8852b rtw89_8852be)
MARK=/run/airdrop-rtw89-build
variant="${2:-}"
if [ -z "$variant" ]; then
  [ -f "$DRV/rtw89_core-awdl-$KREL.ko" ] && variant=awdl || variant=stock
fi
ko() { echo "$DRV/$1-$variant-$KREL.ko"; }
loaded() { cat "/sys/module/$1/srcversion" 2>/dev/null; }

check() {
  for m in "${STACK[@]}"; do
    [ -f "$(ko "$m")" ] || { echo "missing $(ko "$m") (run scripts/rtw89-build.sh)"; return 1; }
    got="$(modinfo -F vermagic "$(ko "$m")" | awk '{print $1}')"
    [ "$got" = "$KREL" ] || { echo "$(ko "$m") is built for $got, the kernel is $KREL: rebuild"; return 1; }
  done
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
  if [ -z "$(loaded rtw89_core)" ]; then echo "rtw89: not loaded"; return; fi
  local v found=""
  for v in awdl stock; do
    [ "$(loaded rtw89_core)" = "$(modinfo -F srcversion "$DRV/rtw89_core-$v-$KREL.ko" 2>/dev/null)" ] && found=$v
  done
  if [ "$found" = awdl ]; then
    echo "rtw89: our PATCHED build loaded ($(ls /sys/module/rtw89_core/parameters | tr '\n' ' '))"
  elif [ "$found" = stock ] && [ "$(cat "$MARK" 2>/dev/null)" = stock ]; then
    echo "rtw89: our stock-source rebuild loaded"
  else
    echo "rtw89: the distribution's modules loaded"
  fi
}

case "${1:-status}" in
  load)
    check || exit 1
    [ -e /run/airdrop-helper.state ] && { echo "AirDrop is running; stop it first (or run: airdrop recover)"; exit 1; }
    if [ "$(loaded rtw89_core)" = "$(modinfo -F srcversion "$(ko rtw89_core)")" ] && [ "$(cat "$MARK" 2>/dev/null)" = "$variant" ]; then
      status; exit 0
    fi
    echo "Swapping in the $variant rtw89 build (Wi-Fi drops for a few seconds)..."
    sudo sh -c "
      for m in rtw89_8852be rtw89_8852b rtw89_8852b_common rtw89_pci rtw89_core; do
        [ -d /sys/module/\$m ] && { rmmod \$m || exit 1; }
      done
      for m in ${STACK[*]}; do
        if ! insmod '$DRV/'\$m'-$variant-$KREL.ko'; then
          echo \"loading \$m FAILED - restoring the distribution's driver\" >&2
          for r in rtw89_8852be rtw89_8852b rtw89_8852b_common rtw89_pci rtw89_core; do rmmod \$r 2>/dev/null; done
          modprobe rtw89_8852be
          rm -f '$MARK'
          exit 2
        fi
      done
      umask 022; echo '$variant' > '$MARK'
    "
    rc=$?
    status
    wait_wifi
    exit $rc
    ;;
  revert)
    [ -e /run/airdrop-helper.state ] && { echo "AirDrop is running; stop it first (or run: airdrop recover)"; exit 1; }
    echo "Restoring the distribution's rtw89 (Wi-Fi drops for a few seconds)..."
    sudo sh -c "for m in rtw89_8852be rtw89_8852b rtw89_8852b_common rtw89_pci rtw89_core; do rmmod \$m 2>/dev/null; done
                modprobe rtw89_8852be; rm -f '$MARK'"
    status
    wait_wifi
    ;;
  install-boot)
    sudo sh -c "cat > /etc/systemd/system/airdrop-rtw89.service" <<UNIT
[Unit]
Description=Load the AirDrop build of the Realtek rtw89 Wi-Fi driver
Wants=sys-subsystem-net-devices-$IFACE.device
After=sys-subsystem-net-devices-$IFACE.device NetworkManager.service

[Service]
Type=oneshot
Environment=IFACE=$IFACE
ExecStart=$ROOT/scripts/rtw89-load.sh load
SuccessExitCodes=0 1 2

[Install]
WantedBy=multi-user.target
UNIT
    sudo systemctl daemon-reload && sudo systemctl enable airdrop-rtw89.service
    ;;
  uninstall-boot)
    sudo systemctl disable airdrop-rtw89.service
    sudo rm -f /etc/systemd/system/airdrop-rtw89.service
    sudo systemctl daemon-reload
    ;;
  status)
    status
    ;;
  *)
    echo "usage: $0 load [awdl|stock]|revert|status|install-boot|uninstall-boot"; exit 2
    ;;
esac
