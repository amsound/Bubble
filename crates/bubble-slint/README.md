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

Slint is used under its GPLv3 licence (Bubble is GPL-3.0-or-later, D-38). The look follows
the design canvas "Bubble main window design" (Mac and Windows boards); the standard
widgets use Slint's cupertino style on macOS and fluent elsewhere (chosen in `build.rs`).

## Layout

| File | What |
|---|---|
| `ui/main.slint` | The window: menu bar, busy popover, cart wall, quick edit, dialogs, context menus |
| `ui/tile.slint` | One cart tile, laid out as the egui tile |
| `ui/bar.slint` | Bottom bar: Edit/Show, page tabs, status line, STOP PAGE, STOP ALL |
| `ui/quickedit.slint` | Quick edit panel |
| `ui/theme.slint` | Light/dark colours, the text field that reports focus, flat buttons, pop-up buttons |
| `src/app.rs` | Input → `Intent`s, the per-frame core work, everything shown |
| `src/model.rs` | Snapshot → tiles, tabs, status, quick edit (text from `bubble_core::view`) |
| `src/menu.rs` | `bubble_core::menu` (the egui app's menu model) as Slint `MenuBar` data |
| `src/keys.rs` | Physical keys → hot-key names, the same names the egui app stored |
| `src/window.rs` | Window place, appearance (title bar and menus), the Mac app and Window menus, pointer during OS drags |
| `src/alerts.rs`, `src/savepanel.rs` | The core's prompts as native alerts; the system Save panel for new sessions |
| `src/fonts.rs` | The system UI font as the system draws it (San Francisco Text on macOS) |

Shared with the egui app through `bubble-core`: the menu model and the settings commands
(`menu.rs`), tile text, window title, status line, busy text and cell helpers
(`view.rs`), and `dialogs::reveal`.

## Checklist

Done (main window):

- [x] Cart wall (design "Tile states"): label (typeface, size, alignment from Display;
      "|" or Shift-Return is a line break), number and times in the system font (no
      slashed zero), colour, hotkey keycap, group chip, one icon row after a small output
      badge (amber when rerouted, the reason on hover): trims, loop, stop all, play next,
      mute, hold, file changed, MIDI trigger; speed tag; MISSING / ERROR tags. The state
      border and the progress band are one shape (yellow playing, purple fading, red near
      the end); selection ring; Learn pulses; control buttons. A cart not ready yet is
      dimmed with a small spinner; a cell receiving a file has an outline and an icon.
- [x] Show mode: play on press, right-click = secondary. Edit mode: mouse press plays and
      selects; touch tap selects and plays on release; press-and-hold or right-click
      (Control-click on a Mac) opens quick edit. Shift-drag moves, Option/Alt-drag copies.
      Arrow keys move the selection, Return opens quick edit.
- [x] Hot keys from physical keys through the winit hook; never sent while a text field
      has the keyboard (Esc still fades everything), releases always sent, holds released
      when the window loses focus.
- [x] Quick edit as a popover beside the cart, with an arrow pointing at it (design
      "Quick edit: options as toggle buttons"), fading in: Cart Setup… (disabled until
      ported) and Close; Label (one line, growing with its text), Colour (swatches, other
      colour as #RRGGBB); Gain (−12…+12 dB, 0 in the middle, click the value for 0),
      Output, Fade Out, Group (pop-up buttons opening a native menu, the current choice
      checked), Hotkey (Learn, the key drawn as on the tile); Loop / Play Next / Stop All
      toggle buttons with the tile's icons; Copy, Paste, Replace File…, Delete. Flat
      system-like buttons (Slint's Mac style draws gradients). Right-clicking another
      cell while it is open moves it there (or opens that empty cell's menu).
- [x] Bottom bar: page tabs (the current page is the inverted neutral tab; a page with
      something playing has a yellow bar along its top; click, double-click rename, drag
      to reorder, menu), add page, PAGE STOP (fades), ALL STOP (immediate), status line
      (click for Relink when files are missing), Small/Medium/Large heights with the
      design's text and button sizes (Medium is the first-run default on a touch screen).
- [x] One busy popover, top centre and non-modal, for everything Bubble is busy with:
      loading audio (n of m files), copying into the session (and to which cell),
      opening, importing, converting. Hides itself when done; drag it out of the way.
- [x] Native context menus (NSMenu / Win32) for empty cells and page tabs, opened at the
      pointer: one submenu at most, Delete Page… last (it asks first).
- [x] Empty cells: double-click or menu to assign audio, paste, add a control button.
- [x] Menus from the shared model: native on macOS (with its shortcuts) and Windows
      (shortcut shown after a tab, run by Bubble), in-window elsewhere. Windows still in
      egui are disabled and marked "not in this interface yet". While a text field has the
      keyboard, ⌘/Ctrl X C V Z act on the field.
- [x] Prompts are the system's own alerts (design: "Bubble Native Alerts"): NSAlert on
      macOS with Bubble's icon and destructive buttons in red; TaskDialog on Windows
      (command links for Import; the plain message box if Common Controls 6 is missing).
      Save changes (Quit, New, Open, Import), Delete Page, Recover (one session per
      alert), Import, problems (bold headline, detail, Copy Details), Missing Files
      (then the system Open panel headed "Locate “file”"). A session set to Convert to
      WAV converts without asking. Paste Attributes is an Edit submenu (both interfaces).
      New Session, Save As and Import into a new session use the system Save panel (a
      sheet on a Mac, with "Audio files: Keep as imported / Convert to WAV" in it for a
      new session; on Windows the Save dialog, then an alert asking Audio files). The
      chosen folder becomes the default for the next one. Bubble draws no dialogs.
      Alerts and native menus open after the current handler returns (main dispatch
      queue on macOS): a menu opened inside a Slint timer panicked with a cart playing.
- [x] OS file drop onto a cell via the winit hook, targeted at the cell under the
      pointer (sessions and SpotOn files open/import; audio fills from that cell, or the
      first empty one), with a highlight while dragging.
- [x] Platform audit rules: file dialogs owned by the main window, nothing slow on the
      UI thread (open, jobs, device scans are the core's helper threads), failures shown
      in the status line, one instance (a second launch hands its file over and the window
      comes to the front), restored place checked against the connected screens.
- [x] Window place saved and restored (`Prefs.window`), appearance Follow System / Light /
      Dark (window, title bar and native menus; Follow System tracks the Mac setting
      live), Page Bar Size. View ▸ Interface Size is gone (from both
      interfaces): Page Bar Size and the session's text sizes cover it.
- [x] macOS app menu named Bubble (About, Settings… shown disabled until ported,
      Services, Hide, Quit) and a Window menu (Minimize, Zoom, Bring All to Front). ⌘Q
      goes through Bubble's own quit, so unsaved changes are asked about.
- [x] System font: Slint falls back to Helvetica/Arial before the system face, so the
      window names it. On macOS Slint draws San Francisco's variable font at its default
      optical size (the Display design, ~13% narrower than the system's text); Bubble
      makes static Text-design instances of the installed SFNS.ttf at launch (once per
      macOS font version, in `~/Library/Caches/Bubble/sf-text`, nothing of Apple's is
      shipped) and puts them first in Slint's chain. Large tile labels keep the Display
      design, as macOS would. Windows uses Segoe UI.
- [x] Renderer: femtovg (OpenGL) by default, `BUBBLE_RENDERER=software` (or `skia` on a
      Mac, for comparison); if the window
      cannot open, one relaunch with the other renderer, then a message. The whole window
      is repainted when it is restored or regains focus (stale areas seen with the software
      renderer after minimising).

Checked by hand on macOS 15 (MacBook Air M4, femtovg): open, play, stop, ALL STOP,
Show/Edit, quick edit (layout against the design, arrow, colour, all three toggles on and
off, Output pop-up, label growing to two lines, moving to another cell, Esc), undo, arrow
keys, page rename (menu and double-click), the native tab and empty-cell menus at the
pointer, Delete Page's question, the busy popover while loading, the app and Window menus,
⌘Q with unsaved changes, Light and Dark appearance, the three bar sizes, window place
restored, OS file drops from Finder. Built but not yet tried by hand: the in-window menu
bar. Cross-builds for Windows x64; not yet run on Windows.

Left:

- [ ] Cart Setup (waveform, trims, loop audition, live trims).
- [ ] Outputs and Reassign Cart Outputs.
- [ ] Session Settings (incl. Display and a custom output level) and Page Settings.
- [ ] Speed Bar.
- [ ] Playout Log and Audio Diagnostics.
- [ ] macOS open-file events (double-click a session in Finder, F5) and Windows file
      types; logging out skips the unsaved-changes question (an autosave is kept and
      offered for recovery).
- [ ] Test on Windows (TaskDialog alerts not yet seen on Windows): touch on the Surface, the native Win32 menu (Alt access, dark
      mode), 125/150/200% scale, the software renderer after minimise.
- [ ] Hot-key labels from the current keyboard layout (I2).
- [ ] From the design, not built yet: Alt-key access to the Windows menu bar (held back:
      Alt+letter would clash with hot keys on Alt), an "other colour" picker beyond
      #RRGGBB, a dashed outline for a cell receiving a file (Slint draws no dashes), and
      blur behind popovers (Slint cannot blur; surfaces are solid).
- [ ] The menu bar's own drop-down menus follow macOS's menu bar appearance, not
      Bubble's Light/Dark choice (no app can change that).
