#!/usr/bin/env bash
# Copy the editable UI sources into the directory Tauri embeds (dist/).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$ROOT/dist"
cp "$ROOT/app/index.html" "$ROOT/dist/index.html"
cp "$ROOT/app/settings.html" "$ROOT/dist/settings.html"
if [[ ! -f "$ROOT/dist/logo.png" ]]; then
    cp "$ROOT/app/icons/256x256.png" "$ROOT/dist/logo.png"
fi
echo "synced UI -> $ROOT/dist"
