#!/usr/bin/env bash
# Package the macOS binary as "storingUnicorns.app" inside a drag-to-install
# disk image (.dmg, with a link to /Applications).
#
# Usage: scripts/build-macos-app.sh <binary> <output.dmg>
#
# The bundle is signed ad hoc (no Apple Developer ID): macOS asks for
# confirmation on the first launch (right-click > Open, or System Settings >
# Privacy & Security > Open Anyway). The in-app updater replaces
# Contents/MacOS/storingUnicorns in place (src/updater/mod.rs).
set -euo pipefail

bin="$1"
out="$2"

NAME="storingUnicorns"
EXE="storingUnicorns"
BUNDLE_ID="io.github.ajustor.storingunicorns"
# CFBundleShortVersionString only takes x.y.z.
version=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
version="${version%%-*}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app="$work/dmg/$NAME.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$bin" "$app/Contents/MacOS/$EXE"
sed -e "s/@NAME@/$NAME/g" -e "s/@EXE@/$EXE/g" -e "s/@BUNDLE_ID@/$BUNDLE_ID/g" \
  -e "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
plutil -lint "$app/Contents/Info.plist"

# Icon: every size of the iconset from the 256 px PNG.
iconset="$work/AppIcon.iconset"
mkdir "$iconset"
for size in 16 32 128 256; do
  sips -z "$size" "$size" assets/icon.png --out "$iconset/icon_${size}x${size}.png" > /dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

ln -s /Applications "$work/dmg/Applications"
rm -f "$out"
# hdiutil sometimes fails with "Resource busy" on CI runners: retry.
for attempt in 1 2 3; do
  if hdiutil create -volname "$NAME" -srcfolder "$work/dmg" -fs HFS+ -format UDZO -ov "$out"; then
    break
  fi
  [ "$attempt" = 3 ] && exit 1
  sleep 5
done
hdiutil verify "$out"
ls -l "$out"
