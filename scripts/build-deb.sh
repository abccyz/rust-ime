#!/usr/bin/env bash
# Build a Debian package for rust-ime.
#
#   ./scripts/build-deb.sh            -> target/rust-ime_<ver>_amd64.deb
#
# The package installs the binary, application menu entries and the icon set.
# Build the release binary first (or pass --build).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VER="${VERSION:-$(grep -m1 '^version' "$ROOT/app/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')}"
ARCH="$(dpkg --print-architecture)"
BIN="$ROOT/target/release/rust-ime-app"
STAGE="$ROOT/target/deb-root"
OUT="$ROOT/target/rust-ime_${VER}_${ARCH}.deb"

if [[ "${1:-}" == "--build" || ! -x "$BIN" ]]; then
    echo "==> building release binary"
    (cd "$ROOT" && cargo build --release -p rust-ime-app)
fi

echo "==> staging"
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" \
         "$STAGE/usr/bin" \
         "$STAGE/usr/share/applications" \
         "$STAGE/usr/share/doc/rust-ime"

install -m755 "$BIN" "$STAGE/usr/bin/rust-ime"

# Icon set
for s in 32 64 128 256 512; do
    d="$STAGE/usr/share/icons/hicolor/${s}x${s}/apps"
    mkdir -p "$d"
    install -m644 "$ROOT/app/icons/${s}x${s}.png" "$d/rust-ime.png"
done

# Application menu: launcher + settings
cat > "$STAGE/usr/share/applications/rust-ime.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=rust-ime
GenericName=拼音输入法
Comment=纯 Rust 拼音输入法
Exec=rust-ime
Icon=rust-ime
Terminal=false
Categories=Utility;
StartupNotify=false
EOF

cat > "$STAGE/usr/share/applications/rust-ime-settings.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=rust-ime 设置
Comment=rust-ime 输入法设置中心
Exec=rust-ime --settings
Icon=rust-ime
Terminal=false
Categories=Settings;Utility;
StartupNotify=false
EOF

cat > "$STAGE/usr/share/doc/rust-ime/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: rust-ime
License: MIT
EOF

# Runtime dependencies (resolved from the linked shared objects).
DEPS=""
if command -v dpkg-shlibdeps >/dev/null 2>&1; then
    mkdir -p "$ROOT/target/shlibdeps/debian"
    printf 'Source: rust-ime\n\nPackage: rust-ime\nArchitecture: any\n' \
        > "$ROOT/target/shlibdeps/debian/control"
    DEPS="$(cd "$ROOT/target/shlibdeps" && dpkg-shlibdeps -O "$BIN" 2>/dev/null \
        | sed -n 's/^shlibs:Depends=//p' || true)"
fi
# Runtime dependencies. dpkg-shlibdeps resolves everything linked at build
# time; libayatana-appindicator is dlopen'd by the tray backend, so add it
# explicitly. Fall back to an explicit list when shlibdeps is unavailable.
BASE="libayatana-appindicator3-1"
if [[ -z "$DEPS" ]]; then
    DEPS="libwebkit2gtk-4.1-0, libgtk-3-0t64, libx11-6, libxtst6, $BASE"
else
    DEPS="$BASE, $DEPS"
fi

INSTALLED_SIZE="$(du -sk "$STAGE" | cut -f1)"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: rust-ime
Version: $VER
Section: utils
Priority: optional
Architecture: $ARCH
Installed-Size: $INSTALLED_SIZE
Depends: $DEPS
Maintainer: rust-ime <noreply@localhost>
Description: 纯 Rust 拼音输入法
 轻量拼音输入法：全拼转换、上下文预测、用户词库自学习。
 支持 fcitx5 插件与独立 X11（XRecord）两种接入方式，
 自带设置中心与系统托盘图标。
EOF

echo "==> building package"
dpkg-deb --build --root-owner-group "$STAGE" "$OUT" >/dev/null
echo
dpkg-deb -I "$OUT" | sed -n '1,20p'
echo
ls -lh "$OUT"
