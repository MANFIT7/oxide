#!/bin/bash
# Build Oxide.app + Oxide.dmg for macOS.
set -euo pipefail
cd "$(dirname "$0")/.."

APP="Oxide"
ID="com.oxide.desktop"
DIST="dist"
BIN="target/release/oxide"

echo "▶ building release binary…"
cargo build --release -p oxide-cli

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
</dict></plist>
PLIST

# ad-hoc codesign so Gatekeeper lets the user open it locally
codesign --force --deep --sign - "$APPDIR" 2>/dev/null || true

echo "▶ building $APP.dmg…"
STAGE="$DIST/stage"
mkdir -p "$STAGE"
cp -R "$APPDIR" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DIST/$APP.dmg"
hdiutil create -volname "$APP" -srcfolder "$STAGE" -ov -format UDZO "$DIST/$APP.dmg" >/dev/null
rm -rf "$STAGE"

echo "✓ done → $DIST/$APP.dmg"
ls -lh "$DIST/$APP.dmg"
