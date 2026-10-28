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
scripts/macos-slint-test-bundle.sh path/to/session.bubble  # a throwaway .app with its own home folder
CARGO_PROFILE_RELEASE_STRIP=debuginfo cargo zigbuild --release -p bubble-slint --target x86_64-pc-windows-gnu
```

Design boards for this interface: [docs/design](../../docs/design/README.md).

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
| `ui/cartsetup.slint` | The Cart Setup window: waveform, trim boxes, transport, its own slider, swatches, pop-up pairs |
| `ui/settings.slint` | The Settings and Session Settings windows: sections, switches, the outputs list |
| `src/settings.rs` | Settings (this computer) and Session Settings (this session): what they show, their pop-up choices, the outputs draft |
| `src/cartsetup.rs` | Cart Setup: the draft cart, the waveform view and gestures, the audition, its pop-up choices |
| `src/app.rs` | Input → `Intent`s, the per-frame core work, everything shown |
| `src/model.rs` | Snapshot → tiles, tabs, status, quick edit (text from `bubble_core::view`) |
| `src/menu.rs` | `bubble_core::menu` (the egui app's menu model) as Slint `MenuBar` data |
| `src/keys.rs` | Physical keys → hot-key names, the same names the egui app stored |
| `src/window.rs` | Window place, appearance (title bar and menus), the Mac app and Window menus, pointer during OS drags |
| `src/alerts.rs`, `src/savepanel.rs` | The core's prompts as native alerts; the system Save panel for new sessions |
| `src/colour.rs` | The system colour picker (Colours panel / Colour dialog) |
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
      "Quick edit: options as toggle buttons"), fading in: Cart Setup… (opens the Cart Setup window); Label (one line, growing with its text; Return or "|" for a new line),
      Colour (swatches; any other colour in the system colour picker: the Colours panel,
      followed live, on a Mac, the Colour dialog on Windows); Gain (−12…+12 dB, 0 in the
      middle, double-click the slider or the value for 0 dB); Output, Fade Out, Group
      ("Group 1"; pop-up buttons as wide as their choice on a Mac, opening a native menu
      with the current choice checked); Hotkey (the key drawn as on the tile with its ✕,
      then Learn at the end, as in Cart Setup; Learn takes one key, no modifiers); Loop / Play Next / Stop All toggle buttons with the
      tile's icons; Copy, Paste, Replace File…, Delete. Flat system-like buttons (Slint's Mac style draws gradients). Right-clicking another
      cell while it is open moves it there (or opens that empty cell's menu).
- [x] Bottom bar: page tabs (the current page is the inverted neutral tab; a page with
      something playing has a yellow bar along its bottom; click, double-click rename, drag
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
- [x] Cart Setup (design: "Bubble Cart Setup design") in its own system window, from
      quick edit's Cart Setup… or Cart ▸ Cart Setup… (⌘I / Ctrl+I). Edits go to a draft:
      OK keeps them (one undo step), Cancel or Esc drops them, the close button with
      edits not kept asks in a system alert (Keep / Discard / Cancel). Waveform of the
      visible part with ruler, In/Out (only their handles move them; off screen they
      are tabs at the edge, dragged in from there; dragging into an edge scrolls),
      fade ramps, cursor (click) and playhead; scroll or swipe moves through time,
      ⌘/Ctrl + scroll, pinch (trackpad, touch screen) or ⌘/Ctrl +/− zoom, a
      double-click plays from there, a double-click on the ruler fits. Meter (PPM,
      `bubble_core::meter`). Trim boxes with −/+ (0.01 s) and Set; Space play/pause,
      I and O set In/Out at the playhead or cursor, ←/→ nudge the last handle (Shift
      0.1 s), P plays the last 3 s before Out. What you hear follows the draft while
      auditioning. Sound and Cart settings as in the design; pop-ups open native menus;
      any other colour in the system colour picker; Replace… and Locate… use the Open
      panel. A learned hotkey or MIDI note plays the cart, and its On Retrigger decides
      a second press (Toggle Stop: play/stop; Restart: play/restart).
      Drawn to the Mac mockup: In and Out sit on the box's edges at fit (no margin);
      holding the playhead holds the sound and letting go carries on; the cursor
      hides while playing. Its own sliders: a press on the track jumps there, a
      double-click on the knob resets, Speed is logarithmic with 100% in the middle.
      Playback as three equal buttons, a one-line Label (Return is OK), swatches
      that take on the press, a short Learn prompt (closing cancels Learn), a
      gradient meter, and a window as tall as its content. On a Mac the main menu
      bar stays while Cart Setup is in front (Slint would swap in an empty one),
      and Cut/Copy/Paste/Undo/Redo act on its text fields.
      Opens at 860 × 686 points and no narrower (taller gives the waveform more room). Seven rows a side: Sound
      (Output, Gain with Mute, Pan, Speed, Speed Bar, Fades In/Out, On Retrigger)
      and Cart (Label, Colour, Group, Playback Loop/Play Next/Hold, On Play Stop
      Page/Stop All, Trigger: the hotkey and MIDI note, each with its own ✕, and one Learn at the end, Notes).
      Replace… and Locate… sit in the header by the file. A label's lines show as
      "Audio Track | line 2" in the header, title and Label field, and a typed "|"
      is a new line; only quick edit adds a line break. ⌘. (Stop All Immediately)
      cancels Cart Setup while it is in front. Dragging the playhead scrubs: moving
      plays from there, holding still goes quiet after 120 ms, letting go carries on
      (or stays paused). Top and Tail trims to the first and last sample at or above
      −40 dBFS on any channel.
      A paused voice (speed 0) fades to silence below 5% speed in the engine, rather
      than holding one sample (a DC offset).
- [x] Start: Bubble reopens the last session. With nothing to reopen (a first launch,
      or the last session is gone) the window opens with a native alert: New
      Session…, Open…, Quit. Cancelling the Save or Open panel brings it back, so
      there is always a session to put audio in.
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

- [x] Settings and Session Settings (D-42), each its own window in front of the main
      one, in sections chosen at the top; changes take effect as they are made.
      Settings is this computer's: General (Appearance, Page Bar Size, Clicking a Cart,
      New Sessions In), Audio (Output Level, Safety Limiter, the outputs A, B, … with
      a name, device and channels each, Sample Rate, Buffer Size: outputs, rate and
      buffer wait for Apply, which restarts audio) and MIDI (on/off, All Ports, each
      port). Session Settings is the session's: General (Name, Audio Files, the
      default Fade-Out and On Retrigger for new carts, Ending Warning, Same Hotkey on
      Several, Hotkeys Act On), Tiles (typeface, sizes, alignment), Groups and Pages
      (name, rows and columns with − and +, cart numbers). Bubble ▸ Settings… on a Mac, File ▸ Settings…
      elsewhere; Session Settings is ⌘, / Ctrl+,. There is no Page Settings window:
      the tab menu has Grid Size ▸ (sizes that would cut off carts are greyed) and
      Session ▸ Grid Size does the same for the current page. Cart numbers count on
      across pages. The menus hold commands, not settings (the egui app keeps its
      settings submenus); View keeps Page Bar Size and Appearance for quick use.
- [x] Fixes after the first Windows run (2026-10-25): assigning several files carries
      on into the next empty cells by cart number, across pages, adding pages when
      they run out; a right-click on another cell or tab while a context menu is open
      opens that one's menu (Windows kept the click); the Colour dialog no longer
      leaves Cart Setup's swatch pressed; a cart's own colour shows in the other-colour
      swatch; tool windows open in the middle of the main window; page tabs sit on
      whole pixels; Cart Setup zooms to 10 ms with a finer envelope (16 frames per
      bucket); control buttons have no Cart Setup in quick edit.
- [x] 2026-10-28: the window shows first and audio, MIDI, recovery and the session
      start after its first frame ("Starting audio…"), each step timed in the log;
      Windows manifest (Windows 10/11, per-monitor DPI v2, long paths) and power
      throttling turned off for the process; the iCloud line is gone from the status
      bar (still logged; the alert for online-only audio stays); every window's text
      at 14 px on Windows (13 on a Mac); quick edit: Cart Setup… left and the number
      right, the swatches only (40 px touch targets in two rows on Windows), gain on
      the 32 px slider; Cart Setup's header "Button 12  44.1 kHz • 2 ch • 00:20" and
      On Play as one joined control.
- [ ] Open: edit-mode touch taps missed on the Surface (cause unknown; the two
      guessed tweaks were taken out again).

Checked by hand on macOS 15 (MacBook Air M4, femtovg): open, play, stop, ALL STOP,
Show/Edit, quick edit (layout against the design, arrow, colour, all three toggles on and
off, Output pop-up, label growing to two lines, moving to another cell, Esc), undo, arrow
keys, page rename (menu and double-click), the native tab and empty-cell menus at the
pointer, Delete Page's question, the busy popover while loading, the app and Window menus,
⌘Q with unsaved changes, Light and Dark appearance, the three bar sizes, window place
restored, OS file drops from Finder. Built but not yet tried by hand: the in-window menu
bar. Cross-builds for Windows x64; not yet run on Windows.

Left:

- [ ] Reassign Cart Outputs.
- [ ] Speed Bar.
- [ ] Playout Log and Audio Diagnostics.
- [ ] macOS open-file events (double-click a session in Finder, F5) and Windows file
      types; logging out skips the unsaved-changes question (an autosave is kept and
      offered for recovery).
- [ ] Test on Windows (TaskDialog alerts not yet seen on Windows): touch on the Surface, the native Win32 menu (Alt access, dark
      mode), 125/150/200% scale, the software renderer after minimise.
- [ ] Hot-key labels from the current keyboard layout (I2).
- [ ] From the design, not built yet: Alt-key access to the Windows menu bar (held back:
      Alt+letter would clash with hot keys on Alt), a dashed outline for a cell receiving a file (Slint draws no dashes), and
      blur behind popovers (Slint cannot blur; surfaces are solid).
- [ ] The menu bar's own drop-down menus follow macOS's menu bar appearance, not
      Bubble's Light/Dark choice (no app can change that).
