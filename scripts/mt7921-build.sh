#!/usr/bin/env bash
# Build the patched mt7921e Wi-Fi driver module for the running kernel.
#
#   scripts/mt7921-build.sh            # fetch the kernel's mt76 source, patch, build
#   scripts/mt7921-build.sh --clean    # throw the fetched source away first
#
# Result: vendor/mt76-driver/mt7921e-awdl-$(uname -r).ko, which
# `scripts/mt7921-ratefix.sh load` swaps in until the next reboot. Nothing is installed.
#
# Needs: git, make, the kernel headers of the running kernel (Arch: linux-headers or your
# kernel's -headers package) and the compiler that kernel was built with (clang kernels: LLVM=1,
# detected below). The first run downloads about 300 MB from git.kernel.org: a shallow,
# sparse clone of the stable tree at the tag of your kernel version, mt76 folder only.
# Run it again after every kernel update: a module only loads into the kernel it was built for.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DRV="$ROOT/vendor/mt76-driver"
SRC="$DRV/linux-src"
PATCH="$ROOT/driver/mt7921-awdl.diff"
MT76=drivers/net/wireless/mediatek/mt76
KREL="$(uname -r)"
# 7.2.5-1-something -> v7.2.5 ; 7.3.0-4-something -> v7.3 (kernel.org tags x.y.0 as vx.y)
KVER="${KVER:-$(echo "$KREL" | sed -E 's/^([0-9]+\.[0-9]+(\.[0-9]+)?).*/\1/; s/\.0$//')}"
TAG="v$KVER"
BUILD="/lib/modules/$KREL/build"
OUT="$DRV/mt7921e-awdl-$KREL.ko"

[ -d "$BUILD" ] || { echo "kernel headers for $KREL are missing ($BUILD)" >&2; exit 1; }
[ "${1:-}" = --clean ] && rm -rf "$SRC"

if [ ! -d "$SRC/.git" ] || [ "$(git -C "$SRC" describe --tags 2>/dev/null)" != "$TAG" ]; then
  rm -rf "$SRC"
  mkdir -p "$DRV"
  echo "Fetching $MT76 at $TAG (about 300 MB)..."
  git clone --quiet --depth 1 --filter=blob:none --sparse --branch "$TAG" \
    https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git "$SRC"
  git -C "$SRC" sparse-checkout set "$MT76"
fi

# Always build from the pristine source plus the patch, never from an earlier patched state.
git -C "$SRC" checkout -q -- "$MT76"
git -C "$SRC" apply --check "$PATCH" || {
  echo "driver/mt7921-awdl.diff does not apply to $TAG. It was written against v7.2.5;" >&2
  echo "see driver/README.md for what it changes." >&2
  exit 1
}
git -C "$SRC" apply "$PATCH"

llvm=()
grep -q '^CONFIG_CC_IS_CLANG=y' "$BUILD/.config" 2>/dev/null && llvm=(LLVM=1)
make -C "$BUILD" M="$SRC/$MT76/mt7921" "${llvm[@]}" CONFIG_MT7921_COMMON=m CONFIG_MT7921E=m modules
cp "$SRC/$MT76/mt7921/mt7921e.ko" "$OUT"

want="$KREL"; got="$(modinfo -F vermagic "$OUT" | awk '{print $1}')"
[ "$got" = "$want" ] || { echo "built for $got, kernel is $want" >&2; exit 1; }
echo "built: $OUT"
echo "load it (until the next reboot): scripts/mt7921-ratefix.sh load"
