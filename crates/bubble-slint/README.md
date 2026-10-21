# bubble-slint

Bubble's main window in [Slint](https://slint.dev) 1.18 (D-39), on top of the shared
application core `bubble_core::Core` (D-40). The egui app (`crates/bubble-app`) is
unchanged and still has everything this interface does not have yet. Both use the same
preferences, sessions, autosave and single-instance lock, so only one of the two runs
at a time.

```sh
cargo run -p bubble-slint -- path/to/session.bubble
BUBBLE_RENDERER=software cargo run -p bubble-slint     # no GPU needed
SLINT_NO_MUDA=1 cargo run -p bubble-slint              # in-window menu bar (as on Linux)
CARGO_PROFILE_RELEASE_STRIP=debuginfo cargo zigbuild --release -p bubble-slint --target x86_64-pc-windows-gnu
```

`scripts/release.sh` also builds `Bubble-Slint-<version>-macOS-universal.zip` and
`Bubble-Slint-<version>-Windows-x64.zip`.

Slint is used under its GPLv3 licence (Bubble is GPL-3.0-or-later, D-38).

## Layout

| File | What |
|---|---|
| `ui/main.slint` | The window: menu bar, busy strip, cart wall, quick edit, dialogs, popup menu |
| `ui/tile.slint` | One cart tile, laid out as the egui tile |
| `ui/bar.slint` | Bottom bar: Edit/Show, page tabs, status line, STOP PAGE, STOP ALL |
| `ui/quickedit.slint`, `ui/dialogs.slint` | Quick edit panel; the core's prompts as in-window dialogs |
| `ui/theme.slint` | Light/dark colours, the text field that reports focus, flat buttons |
| `src/app.rs` | Input → `Intent`s, the per-frame core work, everything shown |
| `src/model.rs` | Snapshot → tiles, tabs, status, quick edit (text from `bubble_core::view`) |
| `src/menu.rs` | `bubble_core::menu` (the egui app's menu model) as Slint `MenuBar` data |
| `src/keys.rs` | Physical keys → hot-key names, the same names the egui app stored |
| `src/window.rs` | Window place, title-bar theme, interface size, pointer during OS drags |

Shared with the egui app through `bubble-core`: the menu model and the settings commands
(`menu.rs`), tile text, window title, status line, busy strip and cell helpers
(`view.rs`), and `dialogs::reveal`.

## Checklist

Done (main window):

- [x] Cart wall: label (typeface, size, alignment from Display), number, colour, output
      badge (amber when rerouted or not set up, with the reason on hover), trim marks
      `> < X`, loop, speed, MUTE/HOLD/stop-others/next/changed flags, trigger keycap,
      group chip; time / remaining / MISSING / ERROR / loading…; progress bar (red near the
      end); steady yellow (playing) and purple (fading) borders; selection; learn
      highlight; control buttons (Stop Group/Page/All).
- [x] Show mode: play on press, right-click = secondary. Edit mode: mouse press plays and
      selects; touch tap selects and plays on release; press-and-hold or right-click
      (Control-click on a Mac) opens quick edit. Shift-drag moves, Option/Alt-drag copies.
      Arrow keys move the selection, Return opens quick edit.
- [x] Hot keys from physical keys through the winit hook; never sent while a text field
      has the keyboard (Esc still fades everything), releases always sent, holds released
      when the window loses focus.
- [x] Quick edit in the window: label, colour swatches and #RRGGBB, gain −12…+12 dB
      (0 in the middle, click the value for 0), speed (click for 100%), output, fade-out,
      group, triggers (Learn), loop/hold/mute/play next/stop others, Replace file, Copy,
      Paste over, Clear.
- [x] Bottom bar: tabs (click, double-click rename, drag to reorder, right-click or
      press-and-hold menu: rename, copy/paste page, stop, add, remove), add page,
      STOP PAGE (fades), STOP ALL (immediate), status line (click for Relink when files
      are missing), Small/Medium/Large heights, blue busy strip with progress.
- [x] Empty cells: double-click or menu to assign audio, paste, control buttons.
- [x] Menus from the shared model: native on macOS (with its shortcuts) and Windows
      (shortcut shown after a tab, run by Bubble), in-window elsewhere. Windows still in
      egui are disabled and marked "not in this interface yet". While a text field has the
      keyboard, ⌘/Ctrl X C V Z act on the field.
- [x] Prompts as in-window dialogs: error (selectable, Copy), recover, unsaved changes,
      import choice, WAV conversion, New Session / Save As (Choose… folder), quit with
      unsaved changes, Paste Attributes, Missing Files (Locate…).
- [x] OS file drop onto a cell via the winit hook, targeted at the cell under the
      pointer (sessions and SpotOn files open/import; audio fills from that cell, or the
      first empty one), with a highlight while dragging.
- [x] Platform audit rules: file dialogs owned by the main window, nothing slow on the
      UI thread (open, jobs, device scans are the core's helper threads), failures shown
      in the status line, one instance (a second launch hands its file over and the window
      comes to the front), restored place checked against the connected screens.
- [x] Window place saved and restored (`Prefs.window`), appearance Follow System / Light /
      Dark (window and title bar), Interface Size (Slint scale factor), Page Bar Size.
- [x] Renderer: femtovg (OpenGL) by default, `BUBBLE_RENDERER=software`; if the window
      cannot open, one relaunch with the other renderer, then a message. The whole window
      is repainted when it is restored or regains focus (stale areas seen with the software
      renderer after minimising).

Checked by hand on macOS 15 (MacBook Air M4, femtovg): open, play, stop, STOP ALL,
Show/Edit, quick edit (colour, loop, label, Esc), undo, arrow keys, page rename, tab menu,
quit with unsaved changes, window place restored, OS file drops from Finder (landed on the
cell under the pointer). Built but not yet tried by hand: appearance switching, Interface Size, the in-window menu bar. Cross-builds for
Windows x64; not yet run on Windows.

Left:

- [ ] Cart Setup (waveform, trims, loop audition, live trims).
- [ ] Outputs and Reassign Cart Outputs.
- [ ] Session Settings (incl. Display and a custom output level) and Page Settings.
- [ ] Speed Bar.
- [ ] Playout Log and Audio Diagnostics.
- [ ] macOS open-file events (double-click a session in Finder, F5) and Windows file
      types; the system Quit (⌘Q in the app menu, logout) skips the unsaved-changes
      question (an autosave is kept and offered for recovery).
- [ ] Test on Windows: touch on the Surface, the native Win32 menu (Alt access, dark
      mode), 125/150/200% scale, the software renderer after minimise.
- [ ] Hot-key labels from the current keyboard layout (I2).
