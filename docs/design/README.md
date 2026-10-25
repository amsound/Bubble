# Design boards

Copies of the design canvases made for the Slint interface, kept here for reference.
The live, viewable versions are claude.ai Design artifacts (links below). The
`.dc.html` files are those canvases' boards. They need the Design canvas runtime
(`support.js`) to render, so open the links to see them; the files here are the
record of what was drawn.

What was built since is in [the bubble-slint README](../../crates/bubble-slint/README.md)
checklist. Where they disagree, the README is current.

## Main window — [artifact](https://claude.ai/artifact/7bihaDRX5zNoMWRYtSfaMA)

`main-window/`

| Board | What it shows |
|---|---|
| `Main` | Platform audit and plan: navigation, typography, theming, menus and dialogs, Mac and Windows |
| `MacWindow`, `WinWindow` | The main window on each platform, with the busy popover |
| `Tile`, `Tiles` | Cart tile anatomy and states (playing, fading, missing, learning, loading…) |
| `PageTabs` | Page tab options; chosen: inverted current tab, yellow bar when playing |
| `Bar` | Bottom bar sizes and the busy popover |
| `Menus` | Context menus (empty cell, page tab) |
| `Dialogs` | Dialog button order per platform (later replaced by native alerts) |
| `QuickEdit`, `QuickEditB`, `QuickEditC` | Quick edit options A, B and C |
| `QuickEditFinal`, `QuickEditFinal2` | Quick edit as chosen |

## Cart Setup — [artifact](https://claude.ai/artifact/3773zqsjh1xCBANZg2Jmg8)

`cart-setup/`

| Board | What it shows |
|---|---|
| `Main` | Audit of the egui Cart Setup: what is native and what is drawn |
| `MacSetup`, `WinSetup` | The proposed window on each platform, system controls tagged |
| `Gestures` | Waveform gestures for trackpad, Windows touchpad/mouse and touch screens |

Built differently from these boards after review: seven rows a side (Sound: Output,
Gain with Mute, Pan, Speed, Speed Bar, Fades, On Retrigger; Cart: Label, Colour,
Group, Playback, On Play, Trigger, Notes), Replace… in the header, Bubble's own
sliders, and 860 × 686 points by default. `WinSetup` still shows the earlier layout.

## Native alerts — [artifact](https://claude.ai/artifact/Bcbb2p5ezpnke1aEJmvYPv)

`alerts/`: every native alert Bubble shows, rendered by macOS itself in light and
dark (`render.swift` draws them with NSAlert; `index.html` is the page).
