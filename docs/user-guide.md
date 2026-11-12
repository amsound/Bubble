# Bubble user guide

How to do things in Bubble, menu by menu. The [README](../README.md) has the overview and downloads.

## Using Bubble

Shortcuts are shown for macOS; on Windows use Ctrl for ⌘ and Alt for ⌥. Windows menus are in sentence case ("Stop all now").

| Action | How |
|---|---|
| Start | File → New Session… (⌘N): a name and a location (default the Bubble folder in your home folder, which cloud services do not sync). Creates `Name/Name.bubble` and `Name/Audio/`. |
| Open anything | File → Open… (⌘O) takes Bubble sessions, SpotOn sessions (`.dta`) and packages (`.pkg`). With a session open, a SpotOn file can be added to it (its pages are appended, its audio copied into Audio/) or opened as a new session. |
| Add audio | Drop files on a cell (they land on the cell under the pointer), double-click an empty cell, or right-click → Assign. Every file is copied into the session's Audio folder first, so a session is always one self-contained folder. |
| Save | ⌘S; File → Save Session As… makes a new session folder with copies of its audio. Session → Restore from Backup goes back to an earlier save. |
| Play / stop | Click or touch a cart (again to stop, with its fade), or select it and Cart → Play (⌘P). Left-plays/right-stops can be chosen per computer in Settings ▸ General. |
| Stop | **STOP ALL** in the page bar cuts everything immediately, as does Audio → Stop All Now (⌘. on a Mac, Ctrl+Shift+A on Windows); Audio → Stop All (fade) and **STOP PAGE** fade out. A fading cart shows a purple border. |
| Control buttons | Right-click an empty cell → Control Button → Stop Group (G1…), Stop This Page, Stop All. Red "Stop" tile with its group shown; triggers by click, touch, key or MIDI like any cart. |
| Select / navigate | Click selects (and plays); in edit mode arrow keys move the selection and Return opens quick edit. |
| Quick edit | In edit mode, right-click (Control-click on a Mac) a cart: label, colour, gain, output, fade-out, group, hotkey and MIDI note (each cleared with its own ✕), playback options. |
| Move / duplicate | Shift-drag moves a cart; ⌥ Option-drag duplicates it; Edit → Duplicate Cart (⌘D). |
| Cart Setup | Cart → Cart Setup… (⌘I): the waveform and meter, Trim In / Out (◀ ▶ nudge 10 ms, Shift 100 ms), transport, Sections (Intro, Main and Outro), and the cart's sound (output, gain, pan, speed, fades, play delay up to 999.9 s) and settings (label, colour, start/end behaviour, group, hotkey, MIDI, Outro trigger, audio file, notes). Space plays/pauses, I / O set In / Out; drag the playhead to scrub. The file is held in memory while Cart Setup is open, so auditions never wait for the disk. Changes apply on OK; Cancel discards them. Tiles show `>` (In trimmed), `<` (Out trimmed) or `X` (both). |
| Speed | Speed preset 25–400 % in Cart Setup; Cart → Speed Bar… (⌘B) opens a small window for one cart (up to four open): the slider and 100 % change the speed live, x0 slows to a stop and x1 returns to normal, each over its own time. |
| Triggers | "Learn" in quick edit or Cart Setup, then press a key or send MIDI. Plays and, pressed again, stops. Shown on the tile: the hotkey as a filled keycap, a MIDI note in an outlined chip before it, named as SpotOn names notes (note 0 is C0, 60 is C5). |
| Pages | Tabs in the page bar: click to show, double-click to rename, drag to reorder, right-click for Copy / Paste Page and more. View → Page Bar Size (Large for touch screens). |
| Session settings | Session → Session Settings… (⌘,): audio files (keep as imported or convert to WAV), default fade-out and On Retrigger for new carts, hotkey rules, tile font/sizes/alignment, exclusive groups, each page's name and grid, and Cart Outputs (how many carts use each output; Move… moves them to another). |
| Computer settings | Bubble → Settings… on a Mac, File → Settings… on Windows: appearance, page bar size, how playing carts show their progress (a bar, a clock, or the countdown only) and how long before the end they turn red, clicking a cart, the new-session folder; output level and safety limiter, outputs, sample rate and buffer size; MIDI ports; and Audio status (dropouts by cause, devices, buffers). These belong to the computer, not the session. |
| Outputs | Settings ▸ Audio: add stereo pairs, mono channels or 5.1 blocks per device, alias and order them. Outputs are lettered A, B, … |
| Playout Log | View → Playout Log (⌘L): one row per play with start, end and length; problems such as refused carts and dropouts with their cause. Copy, export as CSV, or clear. |
| Appearance | View → Appearance: Follow System (default), Light or Dark. The main window reopens where it was last left. |
| Show mode | EDIT / SHOW button in the page bar, or ⌘E: Show mode locks editing. |

Sessions are folders: `Name.bubble` (JSON, paths relative to it) and `Audio/`. Move, copy or back up the folder and it opens on another machine or OS.
