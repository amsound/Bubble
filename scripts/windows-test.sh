#!/bin/bash
# Windows testing from a Mac: cross-builds Bubble, copies it to a test PC over SSH and
# runs it on that PC's desktop.
#   BUBBLE_TEST_PC=user@host scripts/windows-test.sh [build|send|run|stop|log [lines]|go]
# "go" is build, send, run. The PC needs the OpenSSH server and this Mac's key.
set -euo pipefail
cd "$(dirname "$0")/.."
PC="${BUBBLE_TEST_PC:?set BUBBLE_TEST_PC=user@host}"
TARGET=x86_64-pc-windows-msvc
DIR='C:\BubbleTest'
SSH=(ssh -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new -o BatchMode=yes "$PC")

build() {
    PATH="/opt/homebrew/opt/llvm/bin:$PATH" \
        CARGO_PROFILE_RELEASE_INCREMENTAL=true CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
        cargo xwin build --release --locked -p bubble-slint --target "$TARGET" 2>&1 |
        grep -vE 'LNK4099|\.pdb|^ +\|?$' | tail -8
}
stop() { "${SSH[@]}" 'taskkill /im Bubble.exe >nul 2>&1 & exit /b 0'; }
send() {
    stop
    "${SSH[@]}" "if not exist $DIR mkdir $DIR"
    scp -q -o BatchMode=yes "target/$TARGET/release/bubble-slint.exe" "$PC:/BubbleTest/Bubble.exe"
}
# A task, because a program started over SSH alone would not reach the desktop.
run() {
    "${SSH[@]}" "schtasks /create /f /tn BubbleTest /sc once /st 23:59 /it /rl limited /tr $DIR\\Bubble.exe >nul && schtasks /run /tn BubbleTest"
}
log() { "${SSH[@]}" "powershell -NoProfile -Command \"Get-Content \$env:APPDATA\\Bubble\\Bubble\\data\\Logs\\bubble.log -Tail ${1:-40}\""; }

case "${1:-go}" in
build) build ;;
send) send ;;
run) run ;;
stop) stop ;;
log) log "${2:-40}" ;;
go) build && send && run ;;
*) echo "unknown: $1" >&2; exit 2 ;;
esac
