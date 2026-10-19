#!/usr/bin/env bash
# Install the rust-ime fcitx5 addon (requires sudo).
#
# Run from the repository root:
#   sudo ./scripts/install-fcitx5-addon.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADDON="$ROOT/crates/fcitx5-addon"

if [[ ! -f "$ADDON/librustime.so" ]]; then
    echo "error: $ADDON/librustime.so not found."
    echo "Build it first:  make -C crates/fcitx5-addon"
    exit 1
fi

LIBDIR="$(pkg-config --variable=libdir Fcitx5Core 2>/dev/null || echo /usr/lib/x86_64-linux-gnu)/fcitx5"
DATADIR="/usr/share/fcitx5"

echo "Installing addon library  -> $LIBDIR/librustime.so"
install -Dm755 "$ADDON/librustime.so" "$LIBDIR/librustime.so"

echo "Installing addon config   -> $DATADIR/addon/rustime.conf"
install -Dm644 "$ADDON/rustime.conf" "$DATADIR/addon/rustime.conf"

echo "Installing input method   -> $DATADIR/inputmethod/rust-ime.conf"
install -Dm644 "$ADDON/rustime-inputmethod.conf" "$DATADIR/inputmethod/rust-ime.conf"

echo
echo "Done. Restart fcitx5:"
echo "  fcitx5-remote -r    # or: fcitx5 -d -r"
echo
echo "Then add 'rust-ime' in fcitx5-configtool (Input Method tab),"
echo "or append it to ~/.config/fcitx5/profile and restart fcitx5."
