# Bubble

A cross-platform audio cart player for live playout, named for the director's call for "bubble". It replaces the Windows-only SpotOn ("TrackPlay") application, keeping its familiar cart-wall workflow with a new engine and interface, and opens SpotOn sessions and packages.

## Download

Builds for macOS and Windows are on the [Releases](../../releases) page.

## Highlights

- **Cart wall** with pages, colours, groups, hot keys and MIDI triggers, fades, loops, trims, speed and pan per cart, and a floating speed bar.
- **Sessions are folders.** Every audio file is copied into the session's `Audio` folder, so a session can be moved, backed up or handed over as one folder and opens on another machine or OS.
- **Plays from memory.** Audio is decoded once (WAV, AIFF, FLAC, MP3, AAC/M4A, ALAC, Ogg Vorbis), resampled to the engine rate (48 kHz by default) and held in memory as 32-bit float, so a trigger never waits for the disk. Optionally, a session converts its audio to WAV when it is added (Session Settings → Audio files), so it opens faster on slower computers.
- **Several outputs.** Stereo pairs, mono channels or 5.1 blocks across one or more devices, lettered A, B, … with aliases.
- **Keeps playing when things go wrong.** If an output device disappears (a Dante Virtual Soundcard switched off, an interface unplugged), carts on other devices carry on, the lost outputs move to the system default output (marked in amber on their tiles), and they switch back when the device returns and nothing is playing. Audio that would not fit in memory is refused with a message rather than running the computer out of memory. Unsaved work is autosaved every 30 seconds and offered for recovery after a crash, and a crash report is kept.
- **SpotOn import.** Opens SpotOn sessions (`.dta`) and packages (`.pkg`) into a new session, or adds them to the open one, with a report of anything that needs attention.

## Using the app

Shortcuts are shown for macOS; on Windows use Ctrl for ⌘ and Alt for ⌥.

| Action | How |
|---|---|
| Start | File → New Session… (⌘N): a name and a location (default ~/Documents/Bubble). Creates `Name/Name.bubble` and `Name/Audio/`. |
| Open anything | File → Open… (⌘O) takes Bubble sessions, SpotOn sessions (`.dta`) and packages (`.pkg`). With a session open, a SpotOn file can be added to it (its pages are appended, its audio copied into Audio/) or opened as a new session. |
| Add audio | Drop files on a cell (they land on the cell under the pointer), double-click an empty cell, or right-click → Assign. Every file is copied into the session's Audio folder first, so a session is always one self-contained folder. |
| Save | ⌘S; File → Save Session As… makes a new session folder with copies of its audio. The window title shows the session and when it was last saved. |
| Play / stop | Click or touch a cart (again to stop, with its fade). Session → Mouse switches to left-plays/right-stops. |
| Stop | **STOP ALL** in the bottom bar cuts everything immediately; Esc and Audio → Stop All (fade) fade out; **STOP PAGE** fades the page. A fading cart shows a purple border. |
| Control buttons | Right-click an empty cell → Control Button → Stop Group (G1…), Stop This Page, Stop All. Red "Stop" tile with its group shown; triggers by click, touch, key or MIDI like any cart. |
| Select / navigate | Click selects (and plays); in edit mode arrow keys move the selection and Return opens quick edit. |
| Quick edit | In edit mode, right-click (Control-click on a Mac) a cart: label, colour, gain, speed, output, fade-out (common lengths in seconds or any value), group, trigger, options. |
| Move / duplicate | Shift-drag moves a cart; ⌥ Option-drag duplicates it; Edit → Duplicate Cart (⌘D). |
| Cart Setup | Cart → Cart Setup… (⌘I): one page with the waveform and PPM meter, Trim In / Out (◀ ▶ nudge 10 ms, Shift 100 ms), transport (to In, loop, play/pause, stop), and the cart's sound (output, gain, pan, speed, fades, playback) and cart settings (label, colour, start/end behaviour, group, trigger, audio file, notes). Space plays/pauses, I / O set In / Out at the playhead or mouse; Top and tail trims silence. Gain and speed are heard live while previewing. Changes apply on OK; Cancel discards them. Tiles show `>` (In trimmed), `<` (Out trimmed) or `X` (both). |
| Speed | Speed preset 25–400 %; Cart → Speed Bar… opens the floating bar (x1, slider, x0; right-click x0/x1 for ramp times). A cart stopped by x0 starts again from its In point. |
| Triggers | "Learn" in quick edit or Cart Setup, then press a key or send MIDI. Plays and, pressed again, stops; change to Play (restart) or Stop per trigger. Shown on the tile. |
| Pages | Tabs along the bottom: click to show, double-click to rename, drag to reorder, right-click for Copy / Paste Page and more. View → Page Bar Size for touch screens. |
| Session settings | Session → Session Settings… (⌘,): name, output level, fades, ending warning, audio files (keep as imported or convert to WAV), tile font/sizes/alignment, exclusive groups. |
| Outputs | Audio → Outputs…: tick stereo pairs, mono channels or 5.1 blocks per device (channels are shared out as a pool), alias and order them; Apply. Outputs are lettered A, B, … in device/channel order unless reordered. Audio → Reassign Cart Outputs… moves carts from one output to another for all pages, this page, a group or a range of buttons. |
| Show mode | EDIT / SHOW button at the bottom left, or ⌘E: Show mode locks editing. |

Sessions are folders: `Name.bubble` (JSON, paths relative to it) and `Audio/`. Move, copy or back up the folder and it opens on another machine or OS.

## More

- [Decisions](docs/decisions.md): what was decided and why.
- [Platform status](docs/platform-status.md): what has been tested where.
