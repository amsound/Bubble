#!/bin/sh
# Wraps the debug bubble-slint in a throwaway .app for testing on macOS, with its own
# home folder so tests never touch your real preferences, recent sessions or autosaves.
# A separate bundle id lets it run beside the real Bubble, and gives it its own
# Accessibility and screen-recording grants.
#
# Usage: scripts/macos-slint-test-bundle.sh [session.bubble]
#   → target/slint-test/BubbleSlintTest.app, then opens it.
# Renderer: BUBBLE_RENDERER=software (or skia) when running this script.
set -e
cd "$(dirname "$0")/.."
cargo build -p bubble-slint
ROOT="$(pwd)/target/slint-test"
APP="$ROOT/BubbleSlintTest.app"
mkdir -p "$ROOT/home" "$ROOT/bin" "$APP/Contents/MacOS"
# Replace, never overwrite in place: a running copy's binary must not change under it.
rm -f "$ROOT/bin/bubble-slint"
cp target/debug/bubble-slint "$ROOT/bin/"
SESSION="${1:+$(cd "$(dirname "$1")" && pwd)/$(basename "$1")}"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>dev.bubble.slinttest</string>
  <key>CFBundleName</key><string>Bubble</string>
  <key>CFBundleExecutable</key><string>run</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
cat > "$APP/Contents/MacOS/run" <<RUN
#!/bin/sh
export HOME="$ROOT/home"
export BUBBLE_RENDERER="${BUBBLE_RENDERER:-}"
exec "$ROOT/bin/bubble-slint" ${SESSION:+"$SESSION"} > "$ROOT/run.log" 2>&1
RUN
chmod +x "$APP/Contents/MacOS/run"
pkill -f "$ROOT/bin/bubble-slint" 2>/dev/null || true
open "$APP"
echo "Running $APP (log: $ROOT/run.log)"
