# Cart player: page bar and grid, design spec (V2)

Hand-off for implementation. This is the settled design. The values below come from an interactive mockup, so they are exact where stated and framework-agnostic (CSS-style units, px). Implement in whatever UI toolkit the app uses and keep the numbers.

## Decisions already made (do not revisit)

1. **Fused tab.** The active page tab is the same colour as the cart grid panel and touches the panel's bottom edge with no gap, so it reads as part of the grid. Inactive tabs are recessed 6px and outlined.
2. **No column alignment.** Each page has its own grid size (for example 6x2, 2x2, 8x4, 5x6), so tabs and buttons must not align to grid columns.
3. **Text tabs, no thumbnails.** Tabs hold the page name only. Names wrap to 2 lines, then clip.
4. **All tabs have one fixed width.** Width never depends on the label.
5. **Active tab accent** is a 3px bar along its bottom edge in the system accent colour (imported, not hardcoded).
6. **Action buttons are compact**, and everything in the bar is vertically centred on one line (see Geometry).
7. **EDIT is an outlined toggle**, not a filled button. It fills when on. PAGE STOP and ALL STOP are the only filled controls.
8. **Red dot** on a tab means that page has a playing cart.

## Layout (top to bottom)

- **Window**: rounded 14px corners, background = strip colour.
- **Grid panel**: fills remaining height. Padding 12px. CSS grid with `repeat(cols, minmax(0, 1fr))` by `repeat(rows, minmax(0, 1fr))`. Cell corner radius 8px.
- **Bottom bar**: height `bandH`, side padding 12px, children separated by 8px, laid out as a flex row:
  `[page tabs…] [+] [flexible spacer] [EDIT] [divider] [PAGE STOP] [ALL STOP]`

## Bar sizes (user setting: small / medium / large)

The bar takes its height from the grid panel, so larger bars leave less room for cells.

| Token | small | medium | large |
|---|---|---|---|
| `tabH` (inactive tab height) | 44 | 56 | 68 |
| `btnH` (EDIT / stops) | 36 | 44 | 52 |
| `tabW` (all tabs) | 108 | 112 | 112 |
| tab text px / line-height | 13 / 14 | 15 / 17 | 17 / 20 |
| button text px | 12 | 14 | 15 |
| button horizontal padding | 14 | 16 | 16 |
| stop icon (square) | 10 | 12 | 12 |
| red dot diameter | 7 | 8 | 9 |

These pixel values are placeholders. Replace them with the app's real small/medium/large bar heights, keeping the formulas below.

## Geometry (derived from the table)

- `bandH = tabH + 12`
- Active tab: height `tabH + 6`, flush to the top of the bar, `padding-top: 6px`, radius `0 0 8px 8px`, no border.
- Inactive tab: height `tabH`, `margin-top: 6px`, radius 8px, 1px border.
- Both tab types: width `tabW`, `flex: 0 1 auto; min-width: 0` so tabs shrink together if the window is narrow.
- Tab label: centred, `-webkit-line-clamp: 2`, line-height as in the table, horizontal padding 10px.
- `+` button: square `tabH × tabH`, `margin-top: 6px`, 1px dashed border, 8px radius.
- EDIT / PAGE STOP / ALL STOP: height `btnH`, `margin-top: (bandH - btnH) / 2`, radius 8px, text weight 700, letter-spacing 0.06em, uppercase.
- Divider (between EDIT and the stops): 1px wide, height `round(btnH * 0.6)`, vertically centred in the band, 4px extra margin each side.
- Result: the centre line of the bar is `bandH / 2`. Inactive tabs, active-tab text, buttons and the divider all sit on it.

## Colours

| Role | Dark | Light |
|---|---|---|
| Bar strip / window background | `#15171b` | `#d9dee6` |
| Grid panel = active tab fill | `#22252b` | `#eef1f5` |
| Empty cell | `#2e3239` | `#d5dae3` |
| Filled cart (mockup value only; carts keep their own colours) | `#3b6ea8` | `#3b6ea8` |
| Cart text | `#ffffff` | `#ffffff` |
| Active tab text | `#ffffff` (weight 600) | `#15171b` (weight 600) |
| Active tab accent bar (3px, inside bottom edge) | system accent (mock: `#0a84ff`) | system accent (mock: `#007aff`) |
| Inactive tab fill / border / text | `#1d2025` / `#626a79` / `#b4bbc6` | `#c9d0db` / `#6f7b90` / `#1f2630` |
| `+` border / icon | `#626a79` / `#b4bbc6` | `#6f7b90` / `#1f2630` |
| EDIT off: border / text | `#f0a030` / `#f0a030` (transparent fill) | `#a65f00` / `#7a4500` (transparent fill) |
| EDIT on: fill / text | `#f0a030` / `#1a1200` | `#f0a030` / `#1a1200` |
| PAGE STOP fill / border / text | `#2b2f36` / `#626a79` / `#e8ebf0` | `#c9d0db` / `#6f7b90` / `#15171b` |
| ALL STOP fill / text | `#b3202a` / `#ffffff` | `#b3202a` / `#ffffff` |
| Divider | `#3a3f48` | `#aab3c1` |
| Playing: tab dot | `#ff6b70` | `#d92d36` |
| Playing: cart ring and dot | `#ff6b70` | `#ff6b70` |

EDIT border width is 2px. The stop buttons and tabs have 1px borders. Keep the shape cues for "playing" (the dot and the ring) in addition to the colour, so state is not carried by colour alone.

## Cells (per page)

Each page has its own `cols x rows`. Gap and text size depend on density. The mockup uses:

| Grid | Gap | Cell text px | Cell padding |
|---|---|---|---|
| 6x2 | 8 | 14 | 10px 12px |
| 2x2 | 10 | 22 | 14px 18px |
| 8x4 | 6 | 12 | 6px 8px |
| 5x6 | 6 | 12 | 6px 8px |

Derive a rule from the available cell size instead of hardcoding by grid. Text is top-left, weight 500, clipped. A playing cart gets a 2px inset ring plus an 8px dot at top-right (6px inset).

## Behaviour

- Click a tab: switch page (the grid changes to that page's own size and contents).
- EDIT: toggles edit mode. It has `aria-pressed` semantics: outlined when off, filled when on.
- PAGE STOP / ALL STOP: transport actions (stop the current page / everything).
- The red dot on a tab shows when that page has a playing cart.
- `+` adds a page.

## Accessibility

- Every control is a real button with an accessible name. A tab's name includes ", playing" when its dot is shown.
- Text contrast was calculated by hand to be at least 4.5:1 for the colours above (not verified with a tool). Inactive-tab borders are about 3:1 against the bar.
- Touch: at `small`, EDIT and the stops are 36px tall, below the usual 44px touch minimum. At `medium` and `large` they meet it. Decide whether `small` is ever used on touchscreens.

## Not designed yet

- **Many pages.** Tabs shrink together, but there is no overflow behaviour for around 8 or more pages (scroll or a page menu).
- **Real accent colour.** The mock values are placeholders for the imported system accent.
- **Window width.** The mockup is 960px wide. Check at the real minimum window width.
- **Long page names.** Names longer than 2 lines clip. Whether to show the full name on hover or long-press is not decided.
