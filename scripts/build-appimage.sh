#!/usr/bin/env bash
# Package the Linux binary as an AppImage: one executable file that shows up
# with its name and icon in the desktop's menus once integrated (AppImageLauncher,
# Gear Lever...) and updates itself in place (src/updater/mod.rs, $APPIMAGE).
#
# Usage: scripts/build-appimage.sh <binary> <output.AppImage>
#
# Downloads a pinned appimagetool (checked against its SHA-256); it fetches
# the AppImage runtime itself. Runs on the release and CI Linux runners.
set -euo pipefail

bin="$1"
out="$2"

APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-x86_64.AppImage"
APPIMAGETOOL_SHA256="46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

appdir="$work/storingUnicorns.AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/256x256/apps"
install -m 755 "$bin" "$appdir/usr/bin/storingUnicorns"
# The runtime starts AppRun; a link keeps argv and the working directory as is.
ln -s usr/bin/storingUnicorns "$appdir/AppRun"
cp packaging/linux/storingUnicorns.desktop "$appdir/storingUnicorns.desktop"
cp packaging/linux/storingUnicorns.desktop "$appdir/usr/share/applications/"
cp assets/icon.png "$appdir/storingUnicorns.png"
cp assets/icon.png "$appdir/usr/share/icons/hicolor/256x256/apps/storingUnicorns.png"
ln -s storingUnicorns.png "$appdir/.DirIcon"

tool="$work/appimagetool"
curl -fsSL --retry 3 -o "$tool" "$APPIMAGETOOL_URL"
echo "$APPIMAGETOOL_SHA256  $tool" | sha256sum -c -
chmod +x "$tool"

# Extract-and-run: CI runners have no FUSE.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$appdir" "$out"
chmod +x "$out"
ls -l "$out"
