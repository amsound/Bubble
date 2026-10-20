#!/bin/sh
# Wraps the release binary in a minimal .app bundle for local testing on macOS.
# Usage: scripts/macos-dev-bundle.sh  → target/release/Bubble.app
set -e
cd "$(dirname "$0")/.."
cargo build --release -p bubble-app
APP=target/release/Bubble.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/bubble "$APP/Contents/MacOS/bubble"
# App icon from the vector logo (assets/logo.svg).
ICON=target/icon
rm -rf "$ICON"
cargo run --release -q -p bubble-icon -- assets/logo.svg "$ICON" >/dev/null
iconutil -c icns "$ICON/Bubble.iconset" -o "$APP/Contents/Resources/Bubble.icns"
VERSION=$(cargo pkgid -p bubble-app | sed 's/.*[#@]//')
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Bubble</string>
  <key>CFBundleDisplayName</key><string>Bubble</string>
  <key>CFBundleIdentifier</key><string>app.bubble.dev</string>
  <key>CFBundleExecutable</key><string>bubble</string>
  <key>CFBundleIconFile</key><string>Bubble</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
echo "$APP"
