#!/usr/bin/env bash
# Build a .deb package for CURB.
# Run from the repo root: ./packaging/build-deb.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION=$(grep '^version' "$REPO_ROOT/Cargo.toml" | head -1 | grep -oP '"\K[^"]+')
PKG="curb_${VERSION}_amd64"
STAGE="/tmp/$PKG"

echo "Building CURB $VERSION .deb…"

# Build release binaries (skip if already built — CI builds first, then calls this)
if [ ! -f "$REPO_ROOT/target/release/curbd" ] || [ ! -f "$REPO_ROOT/target/release/curb" ]; then
    cargo build --release -p curbd -p curb
fi

# Build GUI (skip if already built)
GUI_BIN="$REPO_ROOT/gui/src-tauri/target/release/curb"
if [ ! -f "$GUI_BIN" ]; then
    echo "Building GUI…"
    cd "$REPO_ROOT/gui"
    npm install --prefer-offline
    cargo build --release --manifest-path src-tauri/Cargo.toml
    cd "$REPO_ROOT"
fi

# Stage tree
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN"
mkdir -p "$STAGE/usr/local/bin"
mkdir -p "$STAGE/etc/systemd/system"

# Binaries
install -m755 "$REPO_ROOT/target/release/curbd" "$STAGE/usr/local/bin/curbd"
install -m755 "$REPO_ROOT/target/release/curb"  "$STAGE/usr/local/bin/curb"

# GUI binary + desktop integration
if [ -f "$GUI_BIN" ]; then
    install -m755 "$GUI_BIN" "$STAGE/usr/local/bin/curb-gui"
    mkdir -p "$STAGE/usr/share/applications"
    install -m644 "$REPO_ROOT/packaging/curb-gui.desktop" "$STAGE/usr/share/applications/curb-gui.desktop"
fi

# Icons (needed for Icon=curb in the .desktop file to resolve)
for SIZE in 32 128 256; do
    SRC="$REPO_ROOT/gui/icons/${SIZE}x${SIZE}.png"
    [ -f "$SRC" ] || continue
    DEST="$STAGE/usr/share/icons/hicolor/${SIZE}x${SIZE}/apps"
    mkdir -p "$DEST"
    install -m644 "$SRC" "$DEST/curb.png"
done

# systemd unit
install -m644 "$REPO_ROOT/packaging/curbd.service" "$STAGE/etc/systemd/system/curbd.service"

# DEBIAN control files
cp "$REPO_ROOT/packaging/debian/control"  "$STAGE/DEBIAN/control"
cp "$REPO_ROOT/packaging/debian/postinst" "$STAGE/DEBIAN/postinst"
cp "$REPO_ROOT/packaging/debian/prerm"    "$STAGE/DEBIAN/prerm"

# Build
dpkg-deb --build --root-owner-group "$STAGE" "$REPO_ROOT/$PKG.deb"
echo "Package: $REPO_ROOT/$PKG.deb"
