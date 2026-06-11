#!/usr/bin/env bash
# CURB installer — works on Debian/Ubuntu and Arch Linux.
# Must be run as root (or with sudo).
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
info()  { echo -e "${GREEN}[curb]${NC} $*"; }
warn()  { echo -e "${YELLOW}[curb]${NC} $*"; }
die()   { echo -e "${RED}[curb] ERROR:${NC} $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "Run as root: sudo $0"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="$REPO_ROOT/target/release"
INSTALL_USER="${SUDO_USER:-}"

# Detect distro family for package manager hints
if command -v apt-get &>/dev/null; then DISTRO=debian
elif command -v pacman  &>/dev/null; then DISTRO=arch
else DISTRO=unknown; fi

# ── 1. Build release binaries if not already built ───────────────────────────
if [ ! -f "$BUILD_DIR/curbd" ] || [ ! -f "$BUILD_DIR/curb" ]; then
    info "Building release binaries (this may take a while)…"
    cd "$REPO_ROOT"
    cargo build --release -p curbd -p curb
fi

# ── 2. Create the `curb` system group ────────────────────────────────────────
if ! getent group curb &>/dev/null; then
    groupadd --system curb
    info "Created system group 'curb'."
else
    info "Group 'curb' already exists."
fi

# ── 3. Add the invoking user to the curb group ───────────────────────────────
if [ -n "$INSTALL_USER" ] && [ "$INSTALL_USER" != "root" ]; then
    if ! id -nG "$INSTALL_USER" | grep -qw curb; then
        usermod -aG curb "$INSTALL_USER"
        info "Added '$INSTALL_USER' to group 'curb'."
    else
        info "User '$INSTALL_USER' is already in group 'curb'."
    fi
else
    warn "Could not determine the installing user. Add yourself to the 'curb' group manually: sudo usermod -aG curb \$USER"
fi
# Note: curb-gui and curb binaries have the setgid bit set (group=curb), so they
# work immediately from any terminal or desktop shortcut — no logout required.

# ── 4. Install binaries ───────────────────────────────────────────────────────
install -Dm755 "$BUILD_DIR/curbd" /usr/local/bin/curbd
install -Dm755 "$BUILD_DIR/curb"  /usr/local/bin/curb
# setgid: CLI also needs group curb to reach the socket
chown root:curb /usr/local/bin/curb
chmod g+s       /usr/local/bin/curb
info "Installed /usr/local/bin/curbd and /usr/local/bin/curb (setgid:curb)."

# ── 5. Install GUI binary (if built) ─────────────────────────────────────────
GUI_BIN="$REPO_ROOT/gui/src-tauri/target/release/curb"
# Fallback: older Tauri output name before we renamed the binary
[ -f "$GUI_BIN" ] || GUI_BIN="$REPO_ROOT/gui/src-tauri/target/release/gui"
if [ -f "$GUI_BIN" ]; then
    install -Dm755 "$GUI_BIN" /usr/local/bin/curb-gui
    # GTK refuses setgid — the GUI relies on the user being in the curb group.
    info "Installed /usr/local/bin/curb-gui."
fi

# ── 6. Install .desktop file (GUI launcher) ──────────────────────────────────
DESKTOP_FILE="$REPO_ROOT/packaging/curb-gui.desktop"
if [ -f "$DESKTOP_FILE" ] && [ -f /usr/local/bin/curb-gui ]; then
    install -Dm644 "$DESKTOP_FILE" /usr/share/applications/curb-gui.desktop
    info "Installed desktop launcher."
fi

# ── 7. Install systemd unit ───────────────────────────────────────────────────
install -Dm644 "$REPO_ROOT/packaging/curbd.service" /etc/systemd/system/curbd.service
info "Installed /etc/systemd/system/curbd.service."

# ── 8. Enable and start the daemon ───────────────────────────────────────────
systemctl daemon-reload
systemctl enable --now curbd
info "curbd is enabled and running."

# ── 9. Verify ────────────────────────────────────────────────────────────────
sleep 0.5
if /usr/local/bin/curb ping &>/dev/null; then
    info "✓ curb ping succeeded — installation complete."
else
    warn "curb ping failed. Check: sudo journalctl -u curbd -n 30"
fi

echo ""
echo -e "${GREEN}CURB installed successfully.${NC}"
echo "  curb apps        — live per-application traffic"
echo "  curb app --help  — per-app limits"
echo "  curb-gui         — graphical interface (or launch from app menu)"
