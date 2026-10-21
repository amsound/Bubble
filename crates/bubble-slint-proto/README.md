# bubble-slint-proto

A throwaway prototype of Bubble's main screen in [Slint](https://slint.dev) 1.18.1. It exists
to compare Slint with the current egui/eframe 0.37 UI before deciding whether to port.
Nothing else depends on it; `crates/bubble-app` is unchanged.

## What it does

- Shows the cart wall for any page size: coloured tiles with number, label, output badge
  ("Out A"), time (duration when ready, `-MM:SS` remaining while playing, or
  MISSING/ERROR/loading…), and a progress bar (green, red near the end).
- Bottom bar: Light/Dark switch, page tabs (scroll when there are many), STOP PAGE (fades),
  STOP ALL (immediate).
- Uses the real engine through `bubble_control::Controller`, as `bubble-app` does: opens the
  session given on the command line, plays on the default output device, and triggers on press
  with `pointer_down`/`pointer_up`, so hold-to-play, retrigger and play-next behave as they
  do in the app.
- Mouse: press plays, right-click opens a small menu (Play, Stop, and two placeholders).
  Touch: tap plays on release; press-and-hold (500 ms) opens the same menu. Slint gives touch
  events a non-zero `touch-finger-id`, which is how the two are told apart. This is the egui
  app's edit-mode touch behaviour; the prototype has no edit/show modes.
- Refreshes at 30 fps while something plays or loads, every 500 ms otherwise. Only tiles
  whose content changed are updated, so Slint redraws only those.

```
bubble-slint-proto [--light|--dark] [--renderer=femtovg|software] [--fps=N] <session.bubble>
```

`--renderer=femtovg` (OpenGL, the default) or `--renderer=software` (CPU only, no GPU needed).
`SLINT_BACKEND=winit-software` does the same as the flag. The title bar shows the renderer
and frame rate in use.

## Measurements (macOS)

MacBook Air M4, macOS 15.8, built-in Retina display (2× scale), release builds. Session
`pkg am` (8 pages, 31 audio files), page 4 (5 × 4, 13 carts). "4 playing" = carts 8, 9, 13
and 14 started together; CPU is six 5-second `top` samples after a few seconds of playing,
memory is `footprint <pid>`. The egui app opened at its remembered size, 1372 × 790 points,
about 19% more area than the prototype's 1200 × 760.

| Build | Idle CPU | CPU, 4 playing | Footprint, this session | Footprint, empty session |
|---|---|---|---|---|
| egui, wgpu/Metal (macOS default) | 0.5% | 9.7% | 833–912 MB | 89 MB |
| egui, glow/OpenGL (Windows default) | 0.4% | 8.1% | 918–967 MB | 150 MB |
| Slint, femtovg/OpenGL (default) | 0.3% | 10.0% | 890–944 MB | 117 MB |
| Slint, software | 0.3% | 40.3% | 783–799 MB | 58 MB |
| Slint, software, `--fps=10` | 0.3% | 29.3% | 724–741 MB | — |

"Empty session" is a one-page session with no audio, so it is the UI's own cost (plus an idle
engine). With real sessions, the prepared audio dominates memory whichever UI is used.

What the numbers say:

- **CPU with a GPU: no difference.** Slint/femtovg and egui both cost about 8–10% of one core
  while 4 carts play at 30 fps, and well under 1% idle. Part of that is the audio engine and
  `Controller::runtime()`, which both builds run the same way.
- **Memory: no meaningful saving.** Slint/femtovg uses more than egui/wgpu and less than
  egui/glow. The software renderer is the leanest (58 MB empty), but 30–60 MB is small next to
  the 700–950 MB of audio in a real session.
- **The software renderer on macOS is not representative.** On macOS, softbuffer (which Slint
  uses to show the image) always reports a fresh buffer, so Slint repaints the *whole* window
  every frame, and macOS then colour-converts every frame (a `sample` shows most of the time
  in vImage). That is also why its colours look slightly washed out on the Mac. On Windows,
  softbuffer reuses the buffer, so Slint repaints only what changed: here, four progress bars
  and four clocks. Expect it to be much cheaper there; that is what the Windows test is for.

### Windows (to test on the 2nd-gen i3, the Surface and the VMware VM)

Cross-built with `cargo zigbuild --release --target x86_64-pc-windows-gnu` (as
`scripts/release.sh` does; 25.6 MB with debug info stripped). The zip built for testing
(`dist/Bubble-Slint-proto-Windows-x64.zip`, not in git) holds the exe and three launchers.
Drop a `.bubble` file on one:

- `Run OpenGL.bat`: femtovg. An Intel HD 3000's Windows driver offers OpenGL 3.1; whether
  femtovg runs well on it is part of the test.
- `Run software.bat`: no GPU at all. This is the one to try on the HD 3000 and in VMware.
- `Run software 10fps.bat`: as above, refreshing 10 times a second while playing.

Rebuild it with:

```sh
CARGO_PROFILE_RELEASE_STRIP=debuginfo cargo zigbuild --release -p bubble-slint-proto --target x86_64-pc-windows-gnu
```

On each machine, compare Task Manager CPU and memory with `Bubble.exe` on the same session
and page, idle and with 4 carts playing, and note how smooth the progress bars and button
presses feel. Not tested here: there is no Windows machine or Wine on this Mac.

## Observations from building it

- The `.slint` markup suits a cart wall: tile visuals, states and theming are short and
  declarative (`ui/main.slint`, ~370 lines; `src/main.rs`, ~330 lines including engine
  wiring). Hot reload and the live preview (VS Code extension) would make visual tuning faster
  than recompiling egui code.
- Redraws are driven by property changes, not by a frame loop, which makes "redraw only what
  changed" the default rather than something to engineer.
- Text in Slint is laid out by the toolkit (word wrap, elide). Long labels without spaces still
  break mid-word, like egui.
- Gaps found:
  - **Files dropped from Finder/Explorer are not delivered.** Slint 1.18 has in-app
    drag-and-drop (`DragArea`/`DropArea`), but its winit backend does not pass on OS file drops.
    Dropping audio onto a tile would need the `unstable-winit-030` feature
    (`on_winit_window_event`) and own hit-testing. Unstable API: it may change between Slint
    versions.
  - The software renderer drew no glyph for ☀/☾ (no font fallback for symbols), so the theme
    button uses words. ■ rendered.
  - Getting the window's position and size back for "remember the window's place" also needs
    the winit accessor.
  - macOS native menu bar: Slint has a `MenuBar` (native on macOS, in-window elsewhere), which
    could replace `muda` plus the custom Windows menu bar, but its shortcut handling would need
    checking.

## Licence (closed-source desktop app)

Slint is offered under three licences (`GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR
LicenseRef-Slint-Software-3.0`; texts in the crate's `LICENSES/`):

1. **Royalty-free Desktop, Mobile and Web Applications Licence 2.0.** Free for closed-source
   desktop apps. Condition: attribution, either the `AboutSlint` widget in an About dialog
   reachable from the top-level menu (or the splash screen), or the "Made with Slint" badge
   on a public web page, ideally the download page. Limits: not for **embedded systems** ("a
   computer system designed to perform a specific task within a larger mechanical or
   electrical system"), and the app must not expose Slint's APIs. Bubble on ordinary PCs and
   Surfaces fits. A dedicated playout box sold as hardware might count as embedded; ask
   SixtyFPS before selling one.
2. **GPLv3.** Would require Bubble to be GPLv3. Not an option for a closed-source app.
3. **Slint Software Licence 3.0 (commercial).** Paid, per SixtyFPS's pricing. Needed only for
   embedded use, or to avoid the attribution, or for support.

For Bubble as it is today, option 1 costs nothing but an About-box widget.

## Estimate: porting the whole UI

The egui UI is about 8,100 lines: `app.rs` 4,755, `app/` 1,845 (Outputs, session, Cart Setup
and audio-out panels), plus tile, waveform, meter, menu, fonts and dialogs (~1,500). Engine,
control and project crates need no change; `Snapshot`/`Intent` in `app.rs` already separate
the UI from the controller, which a port would keep.

| Area | Estimate |
|---|---|
| Main window: cart wall with all tile details (triggers, groups, trim/loop/speed flags, meters, selection, learn), bottom bar with page drag-to-reorder, rename and page menu, status line, edit/show modes, keyboard | 4–6 days |
| Quick edit popup (label, colour wheel and swatches, output, gain, fades, loop, retrigger, triggers) | 3–4 days |
| Cart Setup: waveform drawn in Rust into a `SharedPixelBuffer` or `Path`, zoom/scroll, In/Out/loop handles, play-from-here, loop audition, live trims, speed bar | 6–9 days |
| Outputs, project and page settings, MIDI, diagnostics, log, missing-files/collect/convert dialogs | 5–7 days |
| Menus (macOS native and Windows in-window, shortcuts), file dialogs (keep `rfd`), OS file drop (winit accessor), window place, crash/autosave/recovery wiring, fonts, UI scale | 3–5 days |
| Touch pass, light/dark, testing on the low-end machines, fixing what that finds | 3–5 days |
| **Total** | **about 5–7 weeks** for one developer, longer if parts are redesigned on the way |

Lowest-risk path if the Windows numbers justify it: port the main window first behind a
second binary, as this prototype does, and keep Cart Setup and dialogs in egui until each has
a Slint version. Running two toolkits in one process is not practical, so the switch to users
happens once everything is ported.

## Recommendation from the Mac numbers

On this Mac, Slint is no faster and uses no less memory than the current egui app when a GPU is
used, so a port is justified only if (a) the Windows software-renderer test is clearly better
on the HD 3000, Surface and VMware than egui/glow, or (b) the look and touch handling are worth
the 5–7 weeks on their own. For CPU on any UI, lowering the refresh rate while playing (30 → 10–15
fps; the clock changes once a second and the progress bar moves a few pixels) is a cheap win
that also applies to the egui app.
