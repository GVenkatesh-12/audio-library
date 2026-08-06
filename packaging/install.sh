#!/usr/bin/env bash
# Install (or re-install) Audio Library into the current user's session.
#
# * Builds a release binary.
# * Installs it to ~/.local/bin (or /usr/local/bin when run as root).
# * Registers it in the application menu.
# * Installs the tray app icon.
# * Adds an autostart entry so the tray icon appears right after you log in
#   (before the main window is ever opened).
#
# Usage:
#   ./packaging/install.sh
#   ./packaging/install.sh --no-autostart   # skip the login autostart entry
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"
AUTOSTART=1
for arg in "$@"; do
    case "$arg" in
        --no-autostart) AUTOSTART=0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

cd "$REPO_DIR"

echo ">> Building release binary (offline)..."
cargo build --release --offline

# ---- Choose install locations ----------------------------------------------
if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    BIN_DIR="/usr/local/bin"
    DATA_DIR="/usr/local/share"
else
    BIN_DIR="$HOME/.local/bin"
    DATA_DIR="$HOME/.local/share"
fi
mkdir -p "$BIN_DIR"
mkdir -p "$DATA_DIR/applications"
mkdir -p "$DATA_DIR/icons/hicolor/128x128/apps"
mkdir -p "$HOME/.config/autostart"

BIN_PATH="$BIN_DIR/audio-library"

echo ">> Installing binary -> $BIN_PATH"
install -m 0755 "$REPO_DIR/target/release/audio-library" "$BIN_PATH"

echo ">> Installing app icon"
install -m 0644 "$REPO_DIR/resources/icons/audio-library.png" \
    "$DATA_DIR/icons/hicolor/128x128/apps/audio-library.png"

echo ">> Installing application menu entry"
sed "s|^Exec=.*|Exec=$BIN_PATH|" "$SCRIPT_DIR/audio-library.desktop" \
    > "$DATA_DIR/applications/audio-library.desktop"
chmod 0644 "$DATA_DIR/applications/audio-library.desktop"

if [[ "$AUTOSTART" -eq 1 ]]; then
    echo ">> Installing autostart entry -> ~/.config/autostart/audio-library.desktop"
    sed "s|^Exec=.*|Exec=env AUDIO_LIBRARY_BACKGROUND=1 $BIN_PATH|" \
        "$SCRIPT_DIR/audio-library-autostart.desktop" \
        > "$HOME/.config/autostart/audio-library.desktop"
    chmod 0644 "$HOME/.config/autostart/audio-library.desktop"
else
    echo ">> Skipping autostart entry (--no-autostart)"
    rm -f "$HOME/.config/autostart/audio-library.desktop"
fi

# Refresh icon + desktop caches when available.
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -f "$DATA_DIR/icons/hicolor" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$DATA_DIR/applications" 2>/dev/null || true
fi

echo
echo "Done. Audio Library is installed as:"
echo "   binary       $BIN_PATH"
echo "   app menu     $DATA_DIR/applications/audio-library.desktop"
echo "   autostart    $HOME/.config/autostart/audio-library.desktop"
echo
echo "The tray icon will show after your next login. To start it now, run:"
echo "   env AUDIO_LIBRARY_BACKGROUND=1 $BIN_PATH   (tray only)"
echo "   $BIN_PATH                                  (opens the main window + tray)"
echo
