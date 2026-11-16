#!/usr/bin/env bash
# Build OWL (AWDL for Linux) into vendor/owl and optionally install it where the
# root helper looks for it (/usr/local/lib/airdrop/owl, must be root-owned).
#
#   scripts/build-owl.sh            # build only
#   scripts/build-owl.sh --install  # build, then sudo install
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
owl="$root/vendor/owl"
commit=0ee3263f944ea721c355f826aa90d636d012a512

if [[ ! -d "$owl/.git" ]]; then
  git clone https://github.com/seemoo-lab/owl.git "$owl"
fi
git -C "$owl" fetch --depth 1 origin "$commit" 2>/dev/null || true
git -C "$owl" checkout -q "$commit" 2>/dev/null || true
git -C "$owl" submodule update --init --recursive
# Our OWL changes: channel 36, follow the sync master's channel sequence
# (iOS rejects a constant one), and never transmit on a channel the radio is not on.
patch="$root/packaging/owl-airdrop.patch"
if git -C "$owl" apply --check "$patch" 2>/dev/null; then
  git -C "$owl" apply "$patch"
elif ! git -C "$owl" apply -R --check "$patch" 2>/dev/null; then
  echo "cannot apply $patch" >&2; exit 1
fi

# OWL's bundled googletest predates CMake 4's minimum-version policy.
cmake -S "$owl" -B "$owl/build" -DCMAKE_BUILD_TYPE=Release -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -Wno-dev -Wno-deprecated >/dev/null 2>&1
cmake --build "$owl/build" --target owl -j"$(nproc)"
echo "built: $owl/build/daemon/owl"

if [[ "${1:-}" == "--install" ]]; then
  sudo install -Dm755 -o root -g root "$owl/build/daemon/owl" /usr/local/lib/airdrop/owl
  echo "installed: /usr/local/lib/airdrop/owl"
fi
