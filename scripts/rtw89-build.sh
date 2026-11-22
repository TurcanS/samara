#!/usr/bin/env bash
# Build the Realtek rtw89 Wi-Fi driver stack for the running kernel, with driver/rtw89-awdl.diff
# applied when it exists and is not empty (an empty or missing patch builds the stock source:
# the first check that this tree reproduces the distribution's modules).
#
#   scripts/rtw89-build.sh            # fetch the kernel's rtw89 source, patch, build
#   scripts/rtw89-build.sh --clean    # throw the fetched source away first
#
# Result: vendor/rtw89-driver/<module>-awdl-$(uname -r).ko for the 8852BE stack (rtw89_core,
# rtw89_pci, rtw89_8852b_common, rtw89_8852b, rtw89_8852be), which `scripts/rtw89-load.sh load`
# swaps in until the next reboot. Nothing is installed. The whole stack is built from one tree
# and always loaded together: without MODVERSIONS, mixing our rtw89_core with the distribution's
# chip modules would go unnoticed if a shared struct changed.
#
# Needs: git, make, the kernel headers of the running kernel (Arch: linux-headers) and the
# compiler that kernel was built with. The first run downloads a sparse clone of the stable
# tree at the tag of your kernel version (rtw89 folder only). Run it again after a kernel update.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRV="$ROOT/vendor/rtw89-driver"
SRC="$DRV/linux-src"
PATCH="$ROOT/driver/rtw89-awdl.diff"
RTW=drivers/net/wireless/realtek/rtw89
KREL="$(uname -r)"
# 7.3.5-arch1-1 -> v7.3.5 ; 7.3.0-4-x -> v7.3 (kernel.org tags x.y.0 as vx.y)
KVER="${KVER:-$(echo "$KREL" | sed -E 's/^([0-9]+\.[0-9]+(\.[0-9]+)?).*/\1/; s/\.0$//')}"
TAG="v$KVER"
BUILD="/lib/modules/$KREL/build"
STACK=(rtw89_core rtw89_pci rtw89_8852b_common rtw89_8852b rtw89_8852be)

[ -d "$BUILD" ] || { echo "kernel headers for $KREL are missing ($BUILD)" >&2; exit 1; }
[ "${1:-}" = --clean ] && rm -rf "$SRC"

if [ ! -d "$SRC/.git" ] || [ "$(git -C "$SRC" describe --tags 2>/dev/null)" != "$TAG" ]; then
  rm -rf "$SRC"
  mkdir -p "$DRV"
  echo "Fetching $RTW at $TAG..."
  git clone --quiet --depth 1 --filter=blob:none --sparse --branch "$TAG" \
    https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git "$SRC"
  git -C "$SRC" sparse-checkout set "$RTW"
fi

# Always build from the pristine source plus the patch, never from an earlier patched state.
git -C "$SRC" checkout -q -- "$RTW"
git -C "$SRC" clean -qfdx -- "$RTW"
if [ -s "$PATCH" ]; then
  git -C "$SRC" apply --check "$PATCH" || {
    echo "driver/rtw89-awdl.diff does not apply to $TAG; see driver/README.md." >&2
    exit 1
  }
  git -C "$SRC" apply "$PATCH"
  variant=awdl
else
  echo "No patch (driver/rtw89-awdl.diff empty or missing): building the stock source."
  variant=stock
fi

llvm=()
grep -q '^CONFIG_CC_IS_CLANG=y' "$BUILD/.config" 2>/dev/null && llvm=(LLVM=1)
# Only the 8852BE stack: every other chip switched off on the command line.
off=(CONFIG_RTW89_8851B= CONFIG_RTW89_8851BE= CONFIG_RTW89_8851BU= CONFIG_RTW89_8852A= CONFIG_RTW89_8852AE=
     CONFIG_RTW89_8852AU= CONFIG_RTW89_8852BU= CONFIG_RTW89_8852BT= CONFIG_RTW89_8852BTE= CONFIG_RTW89_8852C=
     CONFIG_RTW89_8852CE= CONFIG_RTW89_8852CU= CONFIG_RTW89_8922A= CONFIG_RTW89_8922AE= CONFIG_RTW89_8922AU=
     CONFIG_RTW89_8922D= CONFIG_RTW89_8922DE= CONFIG_RTW89_USB=)
make -C "$BUILD" M="$SRC/$RTW" "${llvm[@]}" "${off[@]}" -j"$(nproc)" modules

for m in "${STACK[@]}"; do
  ko="$SRC/$RTW/$m.ko"
  [ -f "$ko" ] || { echo "missing $ko" >&2; exit 1; }
  got="$(modinfo -F vermagic "$ko" | awk '{print $1}')"
  [ "$got" = "$KREL" ] || { echo "$m built for $got, kernel is $KREL" >&2; exit 1; }
  cp "$ko" "$DRV/$m-$variant-$KREL.ko"
  printf '%-20s %s  (installed: %s)\n' "$m" "$(modinfo -F srcversion "$ko")" "$(modinfo -F srcversion "$m" 2>/dev/null || echo -)"
done
echo "built: $DRV/*-$variant-$KREL.ko"
echo "load them (until the next reboot): scripts/rtw89-load.sh load${variant:+ $variant}"
