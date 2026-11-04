# bubble-slint

Bubble's interface, in [Slint](https://slint.dev) 1.18 (D-39, D-41), on top of the
application core `bubble_core::Core` (D-40). This crate builds the `Bubble` app; it draws
and handles input, and everything else (sessions, audio, MIDI, preferences, autosave)
lives in `bubble-core` and below.

```sh
cargo run -p bubble-slint -- path/to/session.bubble
BUBBLE_RENDERER=software cargo run -p bubble-slint     # no GPU needed (skia on a Mac, for comparison)
SLINT_NO_MUDA=1 cargo run -p bubble-slint              # in-window menu bar (as on Linux)
scripts/macos-slint-test-bundle.sh path/to/session.bubble  # a throwaway .app with its own home folder
BUBBLE_TEST_PC=user@host scripts/windows-test.sh go    # cross-builds the Windows .exe (MSVC, cargo-xwin), sends and runs it
```

Releases are built by GitHub Actions (`.github/workflows/release.yml`) when a `v*` tag is
pushed: a universal macOS app and Windows x64 and arm64 builds, as a draft release.
Every push to `main` updates the **Test build** pre-release the same way.

Slint is used under its GPLv3 licence (Bubble is GPL-3.0-or-later, D-38). The standard
widgets use Slint's cupertino style on macOS and fluent elsewhere (chosen in `build.rs`).
Design boards: [docs/design](../../docs/design/README.md).

## Layout

| File | What |
|---|---|
| `ui/main.slint` | The main window: menu bar, busy popover, cart wall, quick edit, dialogs, context menus |
| `ui/tile.slint` | One cart tile |
| `ui/bar.slint` | The page bar: Edit/Show, page tabs, status line, STOP PAGE, STOP ALL |
| `ui/quickedit.slint` | Quick edit panel |
| `ui/cartsetup.slint` | Cart Setup: waveform, trims, sections, transport, the cart's sound and settings |
| `ui/settings.slint` | Settings (this computer) and Session Settings (this session) |
| `ui/speedbar.slint` | Speed Bar |
| `ui/playoutlog.slint` | Playout Log |
| `ui/theme.slint` | Light/dark colours, the text field that reports focus, buttons, pop-up buttons |
| `src/app.rs` | Input → `Intent`s, the per-frame core work, everything shown |
| `src/model.rs` | Snapshot → tiles, tabs, status, quick edit (text from `bubble_core::view`) |
| `src/cartsetup.rs` | Cart Setup: the draft cart, the waveform view and gestures, the audition |
| `src/settings.rs` | Both settings windows: what they show, their choices, the outputs draft, Audio status |
| `src/speedbar.rs`, `src/playoutlog.rs` | Speed Bar and Playout Log |
| `src/menu.rs` | `bubble_core::menu` as Slint `MenuBar` data; shortcuts outside macOS |
| `src/keys.rs`, `src/hotkeys.rs` | Physical keys → hot-key names (as sessions store them); the hot key thread |
| `src/window.rs` | Window place, appearance, the Mac app and Window menus, pointer during OS drags |
| `src/alerts.rs`, `src/savepanel.rs` | The core's prompts as native alerts; the system Save panel |
| `src/colour.rs` | The system colour picker |
| `src/fonts.rs` | The system UI font as the system draws it (San Francisco Text on macOS) |

## Rules

- **Nothing blocks the interface thread.** Disk, devices, MIDI ports and decoding run on
  the core's helper threads; the interface only reads `Core::snapshot()` and sends
  `Intent`s.
- **Panels are rebuilt only when what they show changes from outside**, so typing in a
  field is never interrupted.
- **Every element on whole device pixels** (femtovg is patched for this, see
  `vendor/README.md`), so lines stay sharp at every Windows scale factor.
- **Platform style.** Title Case menus on a Mac, sentence case elsewhere
  (`bubble_core::menu::t`); macOS runs menu shortcuts itself, elsewhere Bubble does, so they
  never fire while a text field has the keyboard.

## Testing aids

| Variable | Does |
|---|---|
| `BUBBLE_SNAPSHOT=out.bmp` | Saves the window once started, then quits (skips the single-instance check) |
| `BUBBLE_SNAPSHOT_SCENE` | What to show first: `play`, `log`, `speedbar[:play\|:pick-x1]`, `cartsetup[:action,…\|menu]`, `settings:N`, `session:N[:action:row]` |
| `BUBBLE_SNAPSHOT_WAIT` | How long to wait before the snapshot |
| `BUBBLE_RENDERER` | `software` or `skia` instead of femtovg (OpenGL); if the window cannot open, Bubble relaunches once with the other renderer |
| `BUBBLE_CRASH_TEST` | `native`, `overflow`, `edit`, `audio-panic`, `audio-hang`, `play`: checks crash reports and the audio thread's guards |
