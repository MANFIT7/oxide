#!/bin/bash
# Build Oxide.app + Oxide.dmg for macOS.
set -euo pipefail
cd "$(dirname "$0")/.."

APP="Oxide"
ID="com.oxide.desktop"
DIST="dist"
BIN="target/release/oxide"
SIGN_NAME="${OXIDE_SIGN_IDENTITY:-Oxide Dev}"
REQUIRE_SIGNING="${OXIDE_REQUIRE_SIGNING:-0}"

echo "▶ building release binaries (oxide + oxide-term)…"
cargo build --release -p oxide-cli -p oxide-term

echo "▶ assembling $APP.app…"
rm -rf "$DIST"
APPDIR="$DIST/$APP.app"
mkdir -p "$APPDIR/Contents/MacOS" "$APPDIR/Contents/Resources"

# real binary + a launcher that runs it in GUI mode from the user's home
cp "$BIN" "$APPDIR/Contents/MacOS/oxide-bin"
cat > "$APPDIR/Contents/MacOS/$APP" <<'LAUNCH'
#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"
# Finder gives a minimal PATH — add the dirs where codex/claude/node live so
# the CLI providers (and their subprocesses) resolve.
export PATH="$HOME/.superconductor/bin:$HOME/.local/bin:$HOME/.bun/bin:$HOME/.npm-global/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
exec "$DIR/oxide-bin" gui
LAUNCH
chmod +x "$APPDIR/Contents/MacOS/$APP" "$APPDIR/Contents/MacOS/oxide-bin"

# Native GPU terminal (oxide-term) — built above as a workspace member, so it's
# in the shared target/ next to the main binary. Bundle it (the GUI launches it
# via a current_exe-relative path). REQUIRED: fail the dmg rather than silently
# ship an app whose terminal button is inert.
echo "▶ bundling native GPU terminal (oxide-term)…"
if [ ! -x target/release/oxide-term ]; then
  echo "✗ oxide-term missing at target/release/oxide-term — build failed?" >&2
  exit 1
fi
cp target/release/oxide-term "$APPDIR/Contents/MacOS/oxide-term"
chmod +x "$APPDIR/Contents/MacOS/oxide-term"
echo "  ✓ bundled oxide-term"

# icon: logo.png -> oxide.icns
echo "▶ building icon…"
ICONSET="$DIST/oxide.iconset"
mkdir -p "$ICONSET"
for s in 16 32 64 128 256 512 1024; do
  sips -z $s $s logo.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
done
# retina (@2x) variants
cp "$ICONSET/icon_32x32.png"   "$ICONSET/icon_16x16@2x.png"
cp "$ICONSET/icon_64x64.png"   "$ICONSET/icon_32x32@2x.png"
cp "$ICONSET/icon_256x256.png" "$ICONSET/icon_128x128@2x.png"
cp "$ICONSET/icon_512x512.png" "$ICONSET/icon_256x256@2x.png"
cp "$ICONSET/icon_1024x1024.png" "$ICONSET/icon_512x512@2x.png"
iconutil -c icns "$ICONSET" -o "$APPDIR/Contents/Resources/oxide.icns"
rm -rf "$ICONSET" "$DIST/icon_"*

VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/' || echo 0.0.1)"

cat > "$APPDIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>$APP</string>
  <key>CFBundleDisplayName</key><string>$APP</string>
  <key>CFBundleExecutable</key><string>$APP</string>
  <key>CFBundleIdentifier</key><string>$ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleIconFile</key><string>oxide.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSRemovableVolumesUsageDescription</key><string>Oxide needs access to project folders you choose on external or mounted volumes so agents can read and edit that workspace.</string>
  <key>NSNetworkVolumesUsageDescription</key><string>Oxide needs access to project folders you choose on network volumes so agents can read and edit that workspace.</string>
</dict></plist>
PLIST

# Sign with the stable identity when present (scripts/make-cert.sh)
# so macOS TCC "Allow" grants persist across updates; ad-hoc otherwise.
SIGN_ID="$(security find-identity -v -p codesigning 2>/dev/null | grep -m1 "$SIGN_NAME" | awk '{print $2}' || true)"
if [ -n "${SIGN_ID:-}" ]; then
  echo "▶ signing with $SIGN_NAME (stable identity)…"
  codesign --force --sign "$SIGN_ID" --identifier com.oxide.desktop "$APPDIR/Contents/MacOS/oxide-bin"
  codesign --force --sign "$SIGN_ID" --identifier com.oxide.desktop "$APPDIR/Contents/MacOS/$APP"
  codesign --force --deep --sign "$SIGN_ID" --identifier com.oxide.desktop "$APPDIR"
  # Sign the raw release binary too so the OTA-swapped binary keeps the SAME
  # identity (otherwise the first OTA update reverts to ad-hoc and TCC re-asks).
  codesign --force --sign "$SIGN_ID" --identifier com.oxide.desktop "$BIN" 2>/dev/null || true
  codesign --verify --deep --verbose=2 "$APPDIR"
elif [ "$REQUIRE_SIGNING" = "1" ]; then
  echo "✗ required signing identity '$SIGN_NAME' was not found" >&2
  security find-identity -v -p codesigning >&2 || true
  exit 1
else
  echo "⚠ no '$SIGN_NAME' identity in keychain — app will be ad-hoc signed and macOS may re-ask volume permissions after updates." >&2
  codesign --force --deep --sign - "$APPDIR" 2>/dev/null || true
fi

echo "▶ building $APP.dmg…"
STAGE="$DIST/stage"
mkdir -p "$STAGE/.background"
cp -R "$APPDIR" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

# Synara-style install window: dark background + arrow + positioned icons.
python3 scripts/dmg-bg.py "$STAGE/.background/bg.png" || true

RW="$DIST/rw.dmg"
rm -f "$RW" "$DIST/$APP.dmg"

# Detach any stale "Oxide" volume (a previously-opened dmg) so our RW mounts
# under the expected name instead of "Oxide 1" — otherwise the Finder styling
# targets the wrong disk and the window comes out unstyled (huge, no bg).
for v in /Volumes/"$APP"*; do
  [ -d "$v" ] && hdiutil detach "$v" -force >/dev/null 2>&1 || true
done

hdiutil create -volname "$APP" -srcfolder "$STAGE" -ov -format UDRW "$RW" >/dev/null
MOUNT="$(hdiutil attach -readwrite -noverify -noautoopen "$RW" | egrep '/Volumes/' | sed 's/.*\(\/Volumes\/.*\)/\1/' | head -1)"
# Use the ACTUAL mounted volume name (basename), not the hardcoded title.
VOL="$(basename "$MOUNT")"
# Give Finder a moment to register the freshly-mounted volume, otherwise the
# AppleScript `tell disk` races and fails with -1728 (window comes out unstyled).
open "$MOUNT" >/dev/null 2>&1 || true
sleep 3

if [ -n "${MOUNT:-}" ] && [ -f "$MOUNT/.background/bg.png" ]; then
osascript <<APPLESCRIPT || true
tell application "Finder"
  tell disk "$VOL"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set the bounds of container window to {200, 140, 740, 520}
    set vo to the icon view options of container window
    set arrangement of vo to not arranged
    set icon size of vo to 80
    set background picture of vo to file ".background:bg.png"
    set position of item "$APP.app" of container window to {140, 205}
    set position of item "Applications" of container window to {400, 205}
    update without registering applications
    delay 1
    close
  end tell
end tell
APPLESCRIPT
sync
fi

[ -n "${MOUNT:-}" ] && hdiutil detach "$MOUNT" >/dev/null || true
hdiutil convert "$RW" -format UDZO -ov -o "$DIST/$APP.dmg" >/dev/null
rm -f "$RW"
rm -rf "$STAGE"

echo "✓ done → $DIST/$APP.dmg"
ls -lh "$DIST/$APP.dmg"
