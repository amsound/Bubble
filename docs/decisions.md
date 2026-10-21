# Architecture Decision Record

Short entries. Status is one of Accepted, Superseded or Deferred.

## D-01 Rust workspace with an engine crate that knows nothing about UI or projects — Accepted

The engine is driven only by `EngineCommand`s and reports only `EngineEvent`s plus lock-free status atomics. This keeps it testable offline and lets the CLI harness and the app share it.

## D-02 Mixing in our own callback; DirectSound-style per-cart OS buffers are rejected — Accepted

One cpal output stream. All voices mix into it: gain/pan ramps, sample-accurate fades and loops, and a safety limiter. Wait-free command and event transfer uses `rtrb` SPSC rings. Retired `Arc` sample buffers are returned on a third ring and dropped off the audio thread.

## D-03 UI framework: egui/eframe — Accepted

egui was chosen for v1 because:

- An immediate-mode grid with 60 Hz live state is simple and fast to build.
- It is a single code base on macOS, Windows and Linux.
- It has no JS toolchain (Tauri) and no licence decision (Slint).

Accepted cost: weaker accessibility and less native look. The UI is a separate crate, so it can be replaced.

## D-04 v1 asset residency: fully decoded and resampled to the engine rate at load — Accepted (v1), revised by D-37

This is the simplest model that guarantees no I/O or decoding at trigger time. Streaming from disk is deferred until real projects show memory pressure. 48 kHz stereo f32 is about 23 MB per minute. Since D-37 this holds for short files and compressed formats; long WAV/AIFF files are streamed.

## D-05 Retrigger, exclusive groups, stop-others-on-play are executed inside the engine — Accepted

The engine resolves them atomically at the start of a block when it receives `Play`, so there are no races between "is it playing?" queries and commands. The policy enum: `restart`, `overlap`, `toggle_stop`, `ignore`.

## D-06 Control layer shared behind a `Mutex` (non-RT) — Accepted (v1)

The UI thread and the MIDI callback thread both call into `bubble-control` under one mutex. The audio thread never takes it. Lock hold times are kept short: the UI copies a view snapshot instead of rendering under the lock. Revisit if measurements show MIDI latency affected by UI work.

## D-07 Engine sample rate = device rate; assets re-prepared on change — Accepted

Assets are resampled at load to the running stream's rate. On device or rate change the app rebuilds the engine and re-arms carts. Carts show "Loading" until ready.

## D-08 Project format v1 — Accepted

Implemented now: project, settings, outputs (single `main`), assets with fingerprint and absolute hint, pages with cells, carts (label, colour, asset, gain, mute, loop, retrigger, fades, exclusive group, stop-others), bindings (key, MIDI note/CC). Unknown fields are preserved via `#[serde(flatten)]` maps. play_next and hold_to_play are implemented. Trims are edited in the Audio Setup window. Still deferred: play_delay and fade_out_at_end (stored, not acted on).

## D-09 Native menus with muda; in-window menu only on Linux — Accepted (user request, 2026-10-18)

egui cannot draw OS menus. `muda` (from the Tauri project) gives the macOS system menu bar and the Windows window menu. On Linux it requires a GTK window, so the same menu model is drawn with egui there. The UI builds one `Menu` model per frame, and the native menu is rebuilt only when that model changes (hash). Settings that were floating windows (audio device, buffer size, MIDI ports, behaviour options) are now native submenus and check items. Master gain and default fade-out live in the side panel.

## D-10 File dialogs are asynchronous — Accepted (bug fix)

Synchronous `rfd` dialogs ran a nested modal loop inside the egui/winit callback. On macOS this re-entered winit and aborted the app (`panic_cannot_unwind` in `ApplicationDelegate::maybe_queue_event`, two crash reports on 2026-10-18). Dialogs are now created on the UI thread (shown as sheets), awaited on a helper thread, and their results applied as intents on a later frame.

## D-11 Unsaved work survives every exit path — Accepted (bug fix)

A system quit (Dock, AppleScript, logout) skips `close_requested`, so the unsaved-changes dialog never shows. `App::on_exit` writes `<project>.bubble.autosave`, or `Recovered/Untitled-<session>.bubble` for untitled projects. It never overwrites the project itself. On start-up a newer autosave is offered for Restore or Discard. Saving and an explicit "Quit without saving" remove the autosave.

## D-12 Projects are opened off the UI thread — Accepted (bug fix)

Reading a file in a privacy-protected macOS folder can block until the user answers a system prompt. Opening on the UI thread left the app without a window. Project loads now run on a helper thread, with "opening…" in the status bar.

## D-13 Undo = project snapshots — Accepted

The controller pushes a clone of the project before each change (depth 100). Edits with the same key within 1 s coalesce, so a slider drag is one step. Restoring a snapshot re-arms only assets the cache does not have. Projects are small (KB–MB), so snapshots are cheap and much simpler than inverse commands.

## D-14 blake3 in pure-Rust mode — Accepted

The default `blake3` build compiles C/assembly and needed MSVC's `lib.exe` even to type-check for Windows. The `pure` feature removes every native toolchain requirement. Fingerprints hash at most 2 MiB per file, so the speed difference is irrelevant.

## D-15 Legacy `.dta` import is a separate one-shot crate — Accepted

`dta-import` (library + `dta-import` CLI) reads reference sessions (format 99) read-only and writes a normal `*.bubble` project plus a migration report (`<name>.import-report.json` and `.md`). The app never loads `.dta` directly. It rejects any file whose cart records or option blocks do not end where the framing says. Notes:

- The reference "gain offset −10 dB" becomes the project's **master gain** (−10 dB), and cart gains stay as stored. Folding the offset into every cart would give the same output, but cart values would differ from what the operator set, and it would double-attenuate with a −10 dB master default.
- Missing assets keep the drive-mapped path (or the bare file name) as `path`, and the original Windows path as `absolute_hint` with `platform: "windows"`.
- Reference fade law 1 ("logarithmic") maps to `exponential`, and 0 maps to `linear`.
- Hot-keys map to the app's egui key names. Numpad keys become digit keys because egui does not tell them apart.
- Hot-key, MIDI, group, stop-on-play, play-next, mute and pan values are imported, but flagged in the report as unconfirmed until a known-value save confirms their encoding.

## D-16 Variable-speed playback in the mixer — Accepted

Each voice has a fractional read position and a ramped rate (0–4×). It reads with linear interpolation once the rate is not exactly 1. At 1× the integer path is unchanged and bit-exact. Speed changes ramp, so the speed bar can slow a cart to a standstill and spin it back up like a record player. The floating bar copies the reference's layout (manual p. 238–239): `x1` | ◀ slider ▶ | `x0`, up to four bars, actual speed in the title, and an `@` time prefix on the tile. Right-clicking `x0` or `x1` sets the cart's slow-down or speed-up time (`slowdown_ms`, `speedup_ms`; both default to 2 s, and speed-up may be 0 = instant). When `x0` reaches 0 % the cart ends (the controller sees its voices at speed 0 and kills them, which is silent at that point). The next click, touch, hotkey or MIDI play therefore starts from the In point at the preset speed. A press while the cart is still slowing snaps it back to real time (10 ms ramp) and carries on, as the reference's `x1`/scrollbar cancel a slow-down. Right-clicking `x0` or `x1` swaps the bar's contents for a row of times, because a popup menu would be clipped by the small window. The slider is logarithmic over 25–400 %, so 100 % sits in the middle, matching the reference's thumb position at x1. A voice at speed 0 keeps its slot and position. Live speed bar moves are not saved, but the cart's preset speed (25–400 %) is. Pitch changes with speed. Time-stretching would need a different algorithm and was not requested.

## D-17 Per-cart meters come from the audio thread's block peaks — Accepted

The mixer keeps each voice's absolute sample peak (after gain and pan, before master and limiter) in atomics, and the UI swaps them to zero once per frame. The Audio Setup meter applies PPM ballistics in the UI: instant rise and a 20 dB / 1.7 s fall (IEC 60268-10 Type I style), with a 1.5 s peak hold. The colours are as requested: green below −18 dBFS, orange from −18 to −10, red above −10. The audio thread does no allocation or locking for this.

## D-18 Secondary windows are native OS windows — Accepted (UX fix)

Audio Setup, Project Settings, Page Settings, Speed Bar, Log, Diagnostics and Missing Files are egui immediate viewports, so they are real windows with platform title bars. On macOS the close button is on the left, and Cmd+W closes them. Confirmations and errors are modal sheets inside the main window, and the main window is brought to the front when the quit prompt appears. The cart side panel was removed: everything it held is in the Cart Setup window (waveform, meter, sound, cart, triggers), with right-click quick edit for the common settings. Keys pressed while a secondary window has focus still reach cart triggers. Hold-to-play carts are released when no Bubble window has focus, because key-up events cannot be seen then.

## D-19 Learned triggers toggle by default — Accepted (bug fix)

A learned key previously used Play, which restarts a cart. Pressing the hotkey of a looping cart again therefore restarted it, and it never stopped. New triggers now take the action of a mouse click (Toggle in the default mouse mode). Each trigger's action can be changed in the side panel. OS key-repeat events are ignored.

## D-20 Tile typography is a project setting — Accepted

The label typeface, label size, small-text size and alignment are stored in `settings.display`, so a project looks the same on any machine that has the font. Fonts are loaded from known system font files, only listed when present, and the built-in font is the fallback. New projects get 8 unnamed exclusive groups (G1–G8) and a −10 dB master level.

## D-21 System fonts and MM:SS clocks — Accepted (UX)

The interface uses the platform's own font (San Francisco and SF Mono on macOS, Segoe UI and Consolas on Windows, Noto or DejaVu on Linux), loaded from the system font files, with egui's bundled fonts kept as fallbacks for symbols. Tile times read MM:SS (H:MM:SS from an hour). Durations and countdowns round up, so a 0.9 s cart shows 00:01 and a countdown reaches 00:00 as the cart ends. Editing windows show tenths (MM:SS.s).

## D-22 Output level saved in projects — Accepted

The output level stays a project setting, as the reference's gain offset is part of the session. Projects saved before the −10 dB default stored 0 dB explicitly, so they keep 0 dB until it is changed in Audio → Output Level, which shows the current value in its title.

## D-23 Reference packages (`.pkg`) import through `dta-import` — Accepted

A package is the session plus the audio it uses (layout verified against a real 18-button package; notes in the investigation folder, `08-package-format.md`). `dta-import` recognises a package by its contents, extracts each embedded file once into `Audio/` next to the new project (an identical existing file is reused; a different file with the same name gets a numbered name, so nothing is overwritten), and maps every cart to its extracted file by the exact original path before any searching. Cart records and session options use the `.dta` decoders. Button images and converter information in the package are not imported.

## D-24 Multiple outputs: machine-defined letters, one stream per device — Accepted

As in the reference, outputs are set up per machine (Settings → Outputs): tick channel ranges on any device (stereo pairs, single mono channels, or 5.1 groups of six), give them optional aliases and order them. The order gives the letters: Output A, Output B, … A cart stores only its letter, so a project plays on whatever "Output B" is on the machine it is opened on; a letter not set up there falls back to Output A. Every output is shared with other applications (WASAPI shared mode on Windows, CoreAudio and ALSA/PipeWire as normal), never exclusive.

The app opens one stream (and mixer) per distinct device, with as many channels as its outputs need, all at one sample rate: **48 kHz by default** (Settings → Sample Rate), 32-bit float when the device offers it (CoreAudio always does). The mixer works in 32-bit float throughout. Assets are resampled once to that rate, which is why every device must run at it; a device that cannot is reported and its outputs fall back to Output A. Each voice writes to its output's channel range: mono sums L+R at half level each; stereo is the normal L/R path; 5.1 routes a six-channel source channel for channel (other sources to its front L/R). Exclusive groups and stop-others act across devices: the controller sends the matching stop to every other device's mixer. The safety limiter is per device, so outputs that share a device share its limiter.

The project-level `outputs` bus list from an earlier draft was removed; cart `output` is now a letter A–Z. The legacy importer maps a reference cart's output device index to the same letter (flagged unconfirmed) and imports speed (25–400 %) instead of dropping it.

## D-25 Vector-first graphics; one logo source — Accepted (UX)

The logo is a single SVG (`assets/logo.svg`): a play button inside a bubble, with two trailing bubbles. Everything else comes from it: the app draws the mark with vector shapes (`logo.rs`, used on the empty-project welcome card), the window/Dock icon is rendered from the SVG at build time (`build.rs`, pure-Rust `resvg`), and the macOS `.icns` is rendered by `bubble-icon` and packed with `iconutil` in the bundle script. No bitmaps are committed. The interface itself is drawn by egui as vector shapes and text in the system font, so it scales to any display and to View → Interface Size.

## D-26 Sessions are self-contained folders — Accepted (replaces "start untitled")

Like a DAW project: File → New Session asks for a name and location and creates `Name/Name.bubble` with `Name/Audio/`. Every audio file that enters a session (drop, assign, relink, SpotOn import) is copied into `Audio/` on a background thread before it is assigned; an identical file already there is reused, a different one with the same name is numbered, nothing is overwritten, and copies are written to a temporary name first. Opening a session whose audio lives elsewhere collects it into `Audio/` too. Save As creates a new folder with copies. This makes a `.pkg`-style archive unnecessary for day-to-day work: the folder is the package (zip it to hand it over). File → Open accepts `.bubble`, `.dta` and `.pkg`; the SpotOn formats are imported into a new session folder. The window title shows "Bubble - Name > Last saved at 09:00" (or "Unsaved changes …"), replacing the info row under the title.

## D-27 Drops land under the pointer — Accepted (bug fix)

winit 0.30 reports dragged files without a position and macOS sends no mouse moves during a drag, so drops always went to cell 1. The app now reads the system cursor (`mouse_position`) while files hover and on drop, maps it into the window, highlights the target cell, and assigns from there. Verified with a Finder drag onto cell 5.

## D-28 Control buttons — Accepted

A cart can be a control button (`control: stop_group | stop_page | stop_all`): no audio, but label, colour, group and triggers work as for any cart, so a group can be stopped from a tile, key or MIDI note without the reference's silent-file workaround. Stop Group fades every cart in the button's group on every output device. Suggested next: a Fade Group with its own fade time, and Next/Previous Page buttons.

## D-29 Cart Setup edits are a draft — Accepted (UX)

Cart Setup works on a copy: preview (Space or ▶) plays the copy on the cart's own voice, OK keeps it, Cancel discards it, and closing the window with changes asks first. Trim edits can no longer be lost or committed by accident.

## D-30 Operator feedback round (2026-10-20) — Accepted

- File drags use the system cursor throughout (egui's pointer is stale during an OS drag) and redraw continuously while files hover, so the highlight follows and the drop lands under the pointer.
- Tiles: steady yellow border while playing (no pulsing), purple while fading out, progress bar drawn last inside the border; the bottom text row no longer moves; "Out A" comes first, trim marks after it. Looping carts count down to the loop point.
- STOP ALL in the bottom bar stops immediately without fades (Esc still fades).
- Fade lengths are chosen in seconds from a list (zero, 0.1–0.9, 1–5 by 0.5) or typed; fade-out is in quick edit, both in Cart Setup.
- Cart Setup transport: drawn icons; play/pause (pause = speed 0, holding position); to-In and stop cut without fade-out; preview keeps the fade-in. Gain, mute and speed act live on the preview. The window comes to the front whenever it is opened.
- PPM scale: −60 at the bottom, −18 at half height, −10 at two thirds, 0 at the top; green to −18 inclusive, orange to −10, red above. Meter and waveform follow the light/dark theme.
- SpotOn imports can be appended to the open session. Reassign Cart Outputs replaces the reference's Output Assignment dialog.

## D-31 De-click starts; fading loops end — Accepted (audio)

A voice that starts on a non-silent sample (an In point set mid-waveform, a file that does not start at zero) gets a 1.5 ms linear ramp unless the cart already fades in longer; carts starting from silence play sample-exact. A looping cart that is fading out no longer wraps: it plays at most to its Out point and ends (reason "stopped", so play-next does not fire). The de-click length is an engine setting (`EngineConfig::declick_ms`).

Also: PPM −10 dBFS moved to three quarters of the scale; gain sliders are −12 … +12 dB with 0 in the middle (values outside are kept, the knob pins at the end); dialogs share one button bar (separator, primary on the right) and small windows size themselves to their content.

## D-32 Robustness: lost devices, memory, crashes — Accepted (audio, app)

- A failed output stream no longer stops every output: streams on working devices keep running, the lost device's outputs move to the system default output (their carts show the output as an amber badge), and the device list is checked every 2 s. Once the device is back and nothing is playing, its outputs return to it.
- Prepared audio may use up to half the machine's RAM. A file that would not fit is refused with an error instead of exhausting memory (estimated before decoding; checked while decoding when the length is unknown).
- A panic writes a crash report (`Crashes/` in the app's data folder), autosaves unsaved work when the session is reachable, and the next launch says so. `BUBBLE_CRASH_TEST=1` triggers a test crash.

## D-33 Session setting "Audio files: Keep as imported / Convert to WAV" — Accepted (project)

Audio is always played from memory, decoded once when a session opens; playback costs the same either way. "Convert to WAV" moves that work to the moment a file is added: it is converted once to 32-bit float WAV at the engine's sample rate (default 48 kHz), saved next to the original in the session's Audio folder, and the asset points at the WAV, so opening the session later needs no decoding or resampling. Trims are stored in source frames, so they are rescaled for converted assets. Originals are kept, so undo stays consistent. Files already WAV at the engine rate are left alone. Turning the setting on, or opening a session that has it on, offers to convert files that are not converted yet; audio added while it is on is converted straight away.

## D-34 Cart Setup: play from here, loop-point audition, live loop and trims — Accepted (audio, app)

⌥/Alt-click or middle-click in the waveform plays from that point (the reference uses the middle button; trackpads have none), and dragging with either keeps moving it; plain clicks still set In (left) and Out (right). P plays from 3 s before Out, so with Loop on the Out→In join is heard at once. The engine moves a voice with a 2 ms crossfade to a copy of itself at the new position (`Seek`), keeping gain, speed (a paused voice stays paused), pan and looping. While previewing, Loop on/off and In/Out changes apply to the playing voice (`SetLoop`, `SetRegion`); a looping voice left past a new Out goes to In. Restarts and seeks are not logged as endings.

## D-35 Appearance and window place are per-machine preferences — Accepted (app)

View → Appearance offers Follow System (default), Light and Dark; it is a machine preference, not part of a session, so a session looks right on whichever machine opens it. The main window's position, size and maximised state are saved to preferences a second after they stop changing (and on exit) and restored at launch, in logical points so the interface zoom does not shrink or grow the window. A maximised window keeps the place it returns to when un-maximised. Empty cells are drawn a shade lighter in dark mode so they stay visible.

## D-36 Low-end and Windows performance — Accepted (app, audio)

- Renderer: OpenGL (glow) on Windows and Linux, wgpu (Metal) on macOS; `BUBBLE_RENDERER=wgpu|glow` overrides. GPUs without DirectX 12 or Vulkan (e.g. Intel HD 3000) and virtual machines made wgpu draw on the CPU.
- Idle redraw every 5 s (was 250 ms); 30 fps only while something plays or a meter moves. On a machine drawing in software each frame costs real CPU.
- 16-bit audio at the engine rate is held as 16-bit (exact; detected from the decoded samples, so lossy and 24-bit stay float). A 32-file session went from 1.40 GB to 0.98 GB.
- The Windows audio thread is promoted to real-time ("Pro Audio" MMCSS, falling back to time-critical priority) via cpal's `realtime` feature; CoreAudio already runs the callback on a real-time thread.
- Windows uses the in-window menu bar (follows light/dark; Win32 menu bars do not), with its own keyboard shortcuts.
- Touch: in edit mode a tap selects and plays on release, press-and-hold opens quick edit.
- A session or SpotOn file passed at launch (dropped on Bubble.exe, Open With) opens. Error messages are selectable and can be copied.

## D-37 Long files stream from disk; a pre-roll head stays in memory — Accepted (audio)

Holding every file decoded cost about 1 GB for a 32-file session, too much for old 4 GB machines. Like SpotOn (one ~1.3 s buffer per cart, refilled from an open file while playing), Bubble now keeps only the start of long files in memory and streams the rest.

- **What streams.** WAV and AIFF files of 30 s or more (`PrepareOptions::stream_min_secs`). Shorter files (stings, effects) and compressed formats (MP3, AAC, FLAC, Ogg) are still decoded whole at load, as before: simplest and safest, and their seeks are not sample-exact. "Convert to WAV" (D-33) makes any file streamable.
- **Heads.** Each streamed asset keeps a 2 s pre-roll at the engine rate (`Head`, 16-bit when the source is 16-bit at the engine rate) from the file start and from every cart's In point, plus the In point being edited in Cart Setup. A trigger plays from the head at once, never waiting for the disk. After a trim edit the asset is prepared again in the background with the new heads (it stays playable meanwhile). `Playable` is the shared, immutable thing a voice plays: resident buffer or heads + file.
- **Reader.** One disk-reader thread (`Streamer`) serves every voice of every output. A trigger opens a `VoiceStream` on the control thread (allocating its `rtrb` ring, 4 s at the engine rate) and the reader starts filling at once from where the head ends — before the engine even sees the Play. The reader keeps its own decoder and streaming resampler per voice (rubato `Async` sinc, chunked), fills the emptiest ring first, and polls every 4 ms when idle. Several voices of one cart (Overlap, Restart's fade-out) each have their own stream.
- **Jumps.** A loop wrap, `SetRegion` or `Seek` sends the reader a request: an epoch-tagged position in an atomic. The reader seeks, then acknowledges with the number of samples it had written before switching, so the voice drops exactly the stale samples still in the ring. The In head covers each loop wrap while the reader repositions. A seek's copy voice plays from a head if the target is in one, and takes the stream over when the old position has faded (2 ms); a target outside every head waits a few ms for the disk (counted as `stream_wait_frames`, not an underrun).
- **Exact positions.** Resampled streams line up with a whole-file resample: decoding restarts at a source frame whose engine position is a whole number (a multiple of the rate ratio's denominator), 512 frames early so the filter settles. Heads are made by the same decoder, so head and stream join seamlessly; tests check streamed output against in-memory playback through loops, seeks, speed changes (0–400 %, pause) and resampling.
- **Underruns.** The audio thread never blocks, allocates or frees for streaming. A voice whose data is not there plays silence and holds its place (and its fade-in) until the data comes; a voice that is stopping still fades and ends. Underruns are counted in `Diagnostics` (`stream_underruns`, `stream_underrun_frames`) and shown in Audio Diagnostics. A file that turns out shorter than stated (replaced on disk) ends the voice as at its Out point.
- **Waveform.** Cart Setup's overview (and Top and tail's bounds at −40 dBFS) is read from the file once on a background thread when first needed and kept for the session; a flat placeholder shows meanwhile. Not cached on disk: one read per file per session is cheap (a 5-minute WAV in well under a second), and writing cache files beside a user's session would need managing.
- **Memory.** "Audio memory" now counts resident files plus heads. On a replica of the 31-file "pkg am" session (926 MB of WAV, same formats, lengths and trims), the app's `footprint` after loading fell from 748 MB (926 MB of audio held, release build of `main`) to 102 MB (22 MB of heads). Each playing streamed voice adds its ring: 1.5 MB for 4 s of 48 kHz stereo.

Not done: streaming for FLAC/MP3 (needs exact seeks verified per format); thread priority for the reader (normal priority keeps 4 s ahead easily).

## D-38 Licence: GPLv3 or later — Accepted (project)

Bubble is open source under the GNU General Public License, version 3 or (at your option) any later version (`LICENSE`, `license = "GPL-3.0-or-later"` in the workspace). This also lets Bubble use Slint under its GPLv3 licence, with no attribution widget required.

## D-39 Interface toolkit: move from egui to Slint, after disk streaming — Accepted (app)

A Slint prototype of the main screen (`claude/great-nobel-4b1434`, `crates/bubble-slint-proto`) was tried on a Windows Surface with touch: taps and press-and-hold registered reliably in every mode, the interface looked finished and responded well, and the OpenGL (femtovg) renderer performed best; the software renderer is kept as a fallback for machines without usable OpenGL (its partial-redraw glitch after minimising must be fixed by forcing a full redraw on restore). egui stays acceptable on macOS but handles hybrid mouse/touch Windows devices poorly. CPU and memory were similar on macOS; audio dominates memory either way, which disk streaming addresses. The port starts after disk streaming is tested: main window first (cart wall, pages, touch, menus, OS file drop via the winit accessor, window place), then Cart Setup and the settings windows, then egui is retired. Engine, control and project crates are unchanged.

## D-40 Shared application core (`bubble-core`) — Accepted (app)

Everything Bubble does that is not drawing lives in `bubble-core`, so the egui interface and the Slint interface share one implementation: the `Core` type (session open, save, autosave and recovery; background jobs for copying, importing and converting; audio outputs with device fallback and reconnection; MIDI; file dialogs; preferences; the single-instance guard; cloud-folder checks; crash reports; the log) and the `Intent`s an interface sends. An interface draws from `Core::snapshot()`, sends `Intent`s to `Core::apply`, and answers the questions in `Core::prompts` (errors, recovery, import choice, WAV conversion offer, discard confirmation, New Session). Selection changes the core asks for arrive in `Core::select`; `Core::watch_audio` returns when it next needs to run instead of scheduling redraws itself. The egui app holds a `Core` and reaches it through `Deref`. The rules from `docs/platform-audit.md` (dialogs owned by the main window, retrying saves, reporting failures, one instance, work off the interface thread) are implemented once, here.

## D-41 Slint main window (`bubble-slint`) — Accepted (app)

The main window is ported to Slint as a second binary, `crates/bubble-slint` (`Bubble Slint` in the release zips); the egui app stays complete until Cart Setup and the settings windows are ported (checklist in the crate's README).

- **Shared, not copied.** The menu model and its settings commands (`bubble_core::menu`) and what tiles, title, status line and busy strip say (`bubble_core::view`) moved from the egui app into `bubble-core`; both interfaces use them.
- **winit accessor** (`slint` feature `unstable-winit-030`, Slint pinned to 1.18.1): physical keys for hot keys (named as the egui app named them, so sessions keep working), focus loss (releases held carts), OS file drops (dropped at the cell under the pointer, read from the system as in the egui app), the window's place, theme and scale changes.
- **Menus.** Slint's `MenuBar` is native on macOS and Windows. macOS runs its shortcuts; elsewhere Bubble runs them itself so they never fire while a text field has the keyboard. A `MenuBar` cannot choose between an item and a submenu per model row, so each menu is a fixed list of slots (a run of items, then a submenu); separators use Slint's separator title.
- **In-window panels.** Quick edit, the core's prompts and the popup menus are drawn inside the main window (W2), rebuilt from Rust only when what they show changes from outside, so typing in them is never interrupted.
- **Redraw.** A timer ticks at 30 fps while carts play, 10 fps while busy, once a second otherwise; models are updated row by row, so Slint redraws only what changed. The whole window is repainted after a restore (software renderer).
