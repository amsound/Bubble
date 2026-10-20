# Platform Status

Only what was actually done is recorded here. "Type-checked" means `cargo check` succeeded for that target from macOS, without linking or running.

Last updated: 2026-10-18.

| | macOS (arm64) | Windows (x86_64-msvc) | Linux (x86_64-gnu) |
|---|---|---|---|
| Builds | ✅ debug and release | ⚠ type-checked only | ⚠ type-checked only (ALSA headers stubbed) |
| Automated tests (51) | ✅ all pass | ❌ not run | ❌ not run |
| Audio output | ✅ CoreAudio, MacBook Air Speakers, 48 kHz | not run | not run |
| Native menu bar | ✅ system menu bar (muda) | compiled (window menu via muda) | in-window egui menu (by design) |
| MIDI input | compiled; **not tested with hardware** (no ports on the dev machine) | not run | not run |
| File dialogs | ✅ async sheet dialogs | not run | not run |
| Seen on screen | grid, side panel, native menus (File/Edit/Audio/Settings/View), Edit → Add Page, recovery prompt + Restore | — | — |
| Not yet seen on screen | Audio Setup / waveform window, Paste Attributes dialog (built and unit-tested; the screen locked before visual checks) | — | — |
| Packaging | dev `.app` bundle only (`scripts/macos-dev-bundle.sh`, ad-hoc signed) | none | none |

## macOS measurements (CLI harness, built-in speakers, 48 kHz, f32)

| Requested buffer | Callback frames | CoreAudio-reported output latency | Max callback processing | Xruns |
|---|---|---|---|---|
| 64 | 64 | 3.58 ms | 0.004 ms | 0 |
| 128 | 128 | 4.92 ms | 0.006 ms | 0 |
| 333 (non-power-of-two) | 333 | 9.19 ms | 0.017 ms | 0 |
| 480 | 480 | 12.25 ms | 0.029 ms | 0 |

These come from `bubble-cli play … --buffer N` (single runs, 1–3 voices). "Output latency" is the backend's own estimate (callback → playback timestamp). It is **not** an end-to-end trigger-to-sound measurement; that still needs a loopback measurement.

## Known platform issues

- **macOS privacy prompts.** An unsigned dev build reading projects or audio in `~/Documents`, `~/Desktop` or `~/Downloads` triggers a "would like to access files" prompt. Projects are now opened on a background thread, so the window appears while the prompt is pending (it used to stay hidden).
- **macOS system quit** (Dock → Quit, AppleScript, logout) bypasses the in-app unsaved-changes dialog. Unsaved work is now written to `<project>.bubble.autosave`, or to the Recovered folder for untitled projects, and offered for restore on the next launch. Verified with an AppleScript quit.
- **Windows:** WinMM MIDI ports are exclusive per process, and WASAPI shared mode adds latency. Both untested.
- **Linux:** needs ALSA headers to build; PipeWire is used through its ALSA compatibility. Native menus are not used.

## Next validation steps

1. Run `cargo test --workspace` and the app on a Windows 10/11 machine; record device, buffer and latency with `bubble-cli play`.
2. Same on Linux (PipeWire desktop) in a container or VM; `docker` is installed on the dev Mac but the daemon was not running.
3. Test MIDI with a real or virtual port (macOS IAC driver) and measure MIDI → audio with the loopback procedure.
