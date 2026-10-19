#!/usr/bin/env bash
# Build the fcitx5 addon WITHOUT installing system dev packages.
#
# Downloads libfcitx5{core,config,utils}-dev with `apt-get download` (no root),
# extracts them into a local sysroot, and compiles librustime.so. The result is
# embedded into the app (app/addons/librustime.so) for one-click installation.
#
#   ./scripts/build-addon.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${WORK:-$ROOT/target/addon-build}"
SYSROOT="$WORK/sysroot"
OUT="$ROOT/app/addons/librustime.so"

command -v g++ >/dev/null || { echo "error: g++ not found"; exit 1; }

mkdir -p "$WORK" "$ROOT/app/addons"
cd "$WORK"

echo "==> downloading fcitx5 development packages (no sudo)"
apt-get download libfcitx5core-dev libfcitx5config-dev libfcitx5utils-dev

echo "==> extracting to local sysroot"
rm -rf "$SYSROOT"
mkdir -p "$SYSROOT"
for d in ./*.deb; do dpkg-deb -x "$d" "$SYSROOT"; done

echo "==> building Rust static library"
(cd "$ROOT" && cargo build --release -p ime-engine)

echo "==> compiling addon"
g++ -std=c++20 -O2 -fPIC -shared \
    -I "$SYSROOT/usr/include/Fcitx5/Core" \
    -I "$SYSROOT/usr/include/Fcitx5/Config" \
    -I "$SYSROOT/usr/include/Fcitx5/Utils" \
    "$ROOT/crates/fcitx5-addon/src/rustime.cpp" \
    "$ROOT/target/release/libime_engine.a" \
    /usr/lib/x86_64-linux-gnu/libFcitx5Core.so.7 \
    /usr/lib/x86_64-linux-gnu/libFcitx5Config.so.6 \
    /usr/lib/x86_64-linux-gnu/libFcitx5Utils.so.2 \
    -lpthread -ldl -lm \
    -o "$OUT"

echo
echo "built $OUT"
ls -lh "$OUT"
echo "Install it from the app:  rust-ime --install-addon"
