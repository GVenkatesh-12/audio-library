#!/usr/bin/env bash
# Build Audio Library as a Flatpak bundle and install it for the current user.
#
# This is the easy-install path for Ubuntu:
#   1. (optional) drop your icon in packaging/flatpak/logo/logo.png
#   2. ./packaging/flatpak/build-flatpak.sh
#
# On the first run the GNOME runtime + SDK are downloaded from Flathub, so
# you need network access once. Output bundle: packaging/flatpak/
# org.example.AudioLibrary.flatpak
#
# Usage:
#   ./packaging/flatpak/build-flatpak.sh [--no-install]
#     --no-install  build the .flatpak bundle but don't install it
set -euo pipefail

FLATPAK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$FLATPAK_DIR/../.." && pwd)"
APP_ID="org.example.AudioLibrary"
RUNTIME_VERSION="48"
MANIFEST="$FLATPAK_DIR/$APP_ID.yaml"
ASSETS="$FLATPAK_DIR/assets"
LOGO_DIR="$FLATPAK_DIR/logo"
STAGE="$FLATPAK_DIR/src-stage"
BUILD_DIR="$FLATPAK_DIR/build"
BUNDLE="$FLATPAK_DIR/$APP_ID.flatpak"
INSTALL=1

for arg in "$@"; do
    case "$arg" in
        --no-install) INSTALL=0 ;;
        *) echo "Unknown argument: $arg" >&2; exit 1 ;;
    esac
done

command -v flatpak-builder >/dev/null 2>&1 || {
    echo "flatpak-builder is not installed." >&2
    echo "Install it with:  sudo apt install flatpak-builder" >&2
    exit 1
}

# ---- Icon: prefer the logo dropped in logo/, fall back to the bundled one --
mkdir -p "$ASSETS"
if [[ -f "$LOGO_DIR/logo.png" ]]; then
    ICON_SRC="$LOGO_DIR/logo.png"
    echo ">> Using logo from $LOGO_DIR/logo.png"
else
    ICON_SRC="$REPO_DIR/resources/icons/audio-library.png"
    echo ">> No logo in $LOGO_DIR/logo.png - using the bundled app icon."
fi
if command -v convert >/dev/null 2>&1; then
    echo ">> Preparing 512x512 icon -> $ASSETS/audio-library.png"
    convert "$ICON_SRC" -background none -resize 512x512 "$ASSETS/audio-library.png"
else
    echo ">> Copying icon -> $ASSETS/audio-library.png (ImageMagick missing, not resized)"
    cp "$ICON_SRC" "$ASSETS/audio-library.png"
fi

# ---- Make sure the GNOME runtime + SDK are installed ------------------------
ARCH="$(flatpak --default-arch)"
if ! flatpak list --runtime 2>/dev/null | grep -q "org.gnome.Platform/$ARCH/$RUNTIME_VERSION"; then
    echo ">> Downloading GNOME runtime + SDK (first build, this can take a while)..."
    flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
    flatpak install --user -y flathub \
        "org.gnome.Sdk//$RUNTIME_VERSION" \
        "org.gnome.Platform//$RUNTIME_VERSION"
fi

# ---- Stage the source tree (keep target/ and .git out of the build) --------
echo ">> Staging source tree -> $STAGE"
rm -rf "$STAGE"
mkdir -p "$STAGE"
tar -C "$REPO_DIR" \
    --exclude=.git --exclude=target \
    --exclude=./packaging/flatpak/build \
    --exclude=./packaging/flatpak/src-stage \
    --exclude=*.flatpak \
    -cf - . | tar -C "$STAGE" -xf -

# ---- Build + bundle ---------------------------------------------------------
echo ">> Building $APP_ID..."
flatpak-builder --force-clean --disable-cache --repo="$BUILD_DIR/repo" "$BUILD_DIR" "$MANIFEST"

echo ">> Creating bundle -> $BUNDLE"
flatpak build-bundle "$BUILD_DIR/repo" "$BUNDLE" "$APP_ID" stable

if [[ "$INSTALL" -eq 1 ]]; then
    echo ">> Installing for the current user..."
    flatpak install --user -y "$BUNDLE"
    echo
    echo "Done. Launch Audio Library from the app grid, or run:"
    echo "   flatpak run $APP_ID"
else
    echo
    echo "Bundle ready: $BUNDLE"
    echo "Install it later with:  flatpak install --user $BUNDLE"
fi