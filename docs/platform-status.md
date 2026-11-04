# Platform Status

Only what was actually done is recorded here.

Last updated: 2026-11-04 (0.2.6).

| | macOS | Windows x64 | Windows arm64 | Linux |
|---|---|---|---|---|
| Builds | ✅ universal (Apple silicon + Intel), GitHub Actions and locally | ✅ MSVC, GitHub Actions and cross-built from the Mac (`scripts/windows-test.sh`) | ✅ MSVC, GitHub Actions | not built |
| Automated tests | ✅ all pass (dev Mac, CI) | ✅ all pass (CI, before every release) | built, not run | not run |
| Run by hand | ✅ dev Mac (Apple silicon) | ✅ ThinkPad (Intel) and Surface Go (touch, low-end) | not run on hardware | — |
| Menus | ✅ system menu bar | ✅ window menu (sentence case) | — | in-window (by design) |
| Packaging | zip of an ad-hoc signed `.app`, not notarised | zip with `Bubble.exe`, not signed (SmartScreen asks) | same as x64 | none |

## macOS measurements (CLI harness, built-in speakers, 48 kHz, f32)

| Requested buffer | Callback frames | CoreAudio-reported output latency | Max callback processing | Xruns |
|---|---|---|---|---|
| 64 | 64 | 3.58 ms | 0.004 ms | 0 |
| 128 | 128 | 4.92 ms | 0.006 ms | 0 |
| 333 (non-power-of-two) | 333 | 9.19 ms | 0.017 ms | 0 |
| 480 | 480 | 12.25 ms | 0.029 ms | 0 |

Measured 2026-10-18 with `bubble-cli play … --buffer N` (single runs, 1–3 voices). "Output latency" is the backend's own estimate (callback → playback timestamp). It is **not** an end-to-end trigger-to-sound measurement; that still needs a loopback measurement.

## Known platform issues

- **macOS privacy prompts.** An unsigned dev build reading projects or audio in `~/Documents`, `~/Desktop` or `~/Downloads` triggers a "would like to access files" prompt. Projects are now opened on a background thread, so the window appears while the prompt is pending (it used to stay hidden).
- **macOS system quit** (Dock → Quit, AppleScript, logout) bypasses the in-app unsaved-changes dialog. Unsaved work is now written to `<project>.bubble.autosave`, or to the Recovered folder for untitled projects, and offered for restore on the next launch. Verified with an AppleScript quit.
- **Windows:** WinMM MIDI ports are exclusive per process (another app holding a port keeps Bubble from it), and listing them can take seconds (10 s seen on the Surface), so they are opened on a helper thread. WASAPI shared mode adds latency; not yet measured. Bubble sets each output device it uses to its mixer rate so Windows does not resample.
- **Linux:** needs ALSA headers to build; PipeWire is used through its ALSA compatibility. Native menus are not used.

## Next validation steps

1. Record device, buffer and latency on the ThinkPad and the Surface with `bubble-cli play`, as for macOS above.
2. Run a Windows arm64 build on ARM hardware.
3. Build and run on Linux (PipeWire desktop) in a container or VM; `docker` is installed on the dev Mac but the daemon was not running.
4. Test MIDI with a real or virtual port (macOS IAC driver) and measure MIDI → audio with the loopback procedure.
