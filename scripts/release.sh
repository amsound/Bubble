#!/bin/sh
# Builds the release downloads into dist/:
#   Bubble-<version>-macOS-universal.zip  (Apple silicon + Intel, ad-hoc signed)
#   Bubble-<version>-Windows-x64.zip      (Bubble.exe, Intel/AMD 64-bit)
#   Bubble-<version>-Windows-arm64.zip    (Bubble.exe, Windows on ARM)
#   SHA256SUMS.txt
# Windows builds use the GNU toolchain via zig: nothing else to install and no
# Microsoft licence to accept.
# Needs: rustup targets aarch64-apple-darwin, x86_64-apple-darwin,
# x86_64-pc-windows-gnu, aarch64-pc-windows-gnullvm; cargo-zigbuild and zig.
set -e
cd "$(dirname "$0")/.."
VERSION=$(cargo pkgid -p bubble-app | sed 's/.*[#@]//')
DIST=dist
mkdir -p "$DIST"

# ---- macOS: one universal app ----
cargo build --release -p bubble-app --target aarch64-apple-darwin
cargo build --release -p bubble-app --target x86_64-apple-darwin
APP="$DIST/Bubble.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create \
  target/aarch64-apple-darwin/release/bubble \
  target/x86_64-apple-darwin/release/bubble \
  -output "$APP/Contents/MacOS/bubble"
ICON=target/icon
rm -rf "$ICON"
cargo run --release -q -p bubble-icon -- assets/logo.svg "$ICON" >/dev/null
iconutil -c icns "$ICON/Bubble.iconset" -o "$APP/Contents/Resources/Bubble.icns"
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
# Extended attributes (provenance, Finder info) make codesign refuse the bundle.
xattr -cr "$APP"
codesign --force --sign - "$APP"
MAC_ZIP="$DIST/Bubble-$VERSION-macOS-universal.zip"
rm -f "$MAC_ZIP"
ditto -c -k --keepParent "$APP" "$MAC_ZIP"

# ---- Windows: Bubble.exe in a folder, per architecture ----
for pair in x64:x86_64-pc-windows-gnu arm64:aarch64-pc-windows-gnullvm; do
  arch=${pair%%:*}
  target=${pair#*:}
  # The GNU toolchain keeps DWARF inside the .exe (~200 MB); strip it, keeping
  # symbol names so crash reports stay readable.
  CARGO_PROFILE_RELEASE_STRIP=debuginfo cargo zigbuild --release -p bubble-app --target "$target"
  WIN="$DIST/Bubble-$VERSION-Windows-$arch"
  rm -rf "$WIN" "$WIN.zip"
  mkdir -p "$WIN"
  cp "target/$target/release/bubble.exe" "$WIN/Bubble.exe"
  (cd "$DIST" && zip -qr "$(basename "$WIN").zip" "$(basename "$WIN")")
  rm -rf "$WIN"
done
rm -rf "$APP"

(cd "$DIST" && rm -f SHA256SUMS.txt && shasum -a 256 Bubble-"$VERSION"-*.zip > SHA256SUMS.txt)
ls -l "$DIST"
