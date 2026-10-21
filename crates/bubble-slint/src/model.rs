//! What the window shows, built from a [`Snapshot`]: tiles, page tabs, the status
//! line, the label style and the quick-edit panel. Text comes from
//! `bubble_core::view`, so it reads the same as in the egui app.

use bubble_control::{CartState as Cs, display_label};
use bubble_core::Snapshot;
use bubble_core::app::Core;
use bubble_core::view::{Level, group_label, tile_text, trigger_names};
use bubble_project::{Cart, Display, HAlign, StopScope, VAlign};
use slint::{Color, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use crate::{CartState, LabelStyle, PageTab, QuickEdit, StatusItem, Tile};

pub const DEFAULT_CART_COLOR: [u8; 3] = [0x3A, 0x6E, 0xA5];
/// Quick colour choices for tiles (as in the egui quick edit).
pub const SWATCHES: [[u8; 3]; 10] = [
    DEFAULT_CART_COLOR,
    [0x2E, 0x7D, 0x32],
    [0xC6, 0x28, 0x28],
    [0xEF, 0x6C, 0x00],
    [0xF9, 0xA8, 0x25],
    [0x6A, 0x1B, 0x9A],
    [0xAD, 0x14, 0x57],
    [0x00, 0x83, 0x8F],
    [0x5D, 0x40, 0x37],
    [0x54, 0x5B, 0x64],
];
/// Fade lengths offered in quick edit (ms): zero, 0.1–0.9 s, 1–5 s by 0.5 s.
pub const FADE_PRESETS_MS: [u32; 19] = [
    0, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1500, 2000, 2500, 3000, 3500, 4000, 4500,
    5000,
];

pub fn parse_rgb(s: Option<&str>) -> Option<[u8; 3]> {
    let s = s?.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn rgb(c: [u8; 3]) -> Color {
    Color::from_rgb_u8(c[0], c[1], c[2])
}

/// Black or white, whichever reads on `bg`.
fn text_color(c: [u8; 3]) -> Color {
    let l = 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
    if l > 150.0 {
        Color::from_rgb_u8(0, 0, 0)
    } else {
        Color::from_rgb_u8(255, 255, 255)
    }
}

/// What the interface selects: a cart, or (keyboard cursor) an empty cell.
pub struct Selection {
    pub cart: Option<Uuid>,
    pub empty: Option<(usize, u16, u16)>,
}

/// The current page: rows, columns, the cart in each cell, and its tiles.
pub struct PageView {
    pub rows: u16,
    pub cols: u16,
    pub ids: Vec<Option<Uuid>>,
    pub tiles: Vec<Tile>,
}

pub fn page_view(core: &Core, snap: &Snapshot, sel: &Selection) -> PageView {
    let Some(page) = snap.project.pages.get(snap.page) else {
        return PageView {
            rows: 1,
            cols: 1,
            ids: Vec::new(),
            tiles: Vec::new(),
        };
    };
    let mut ids = Vec::new();
    let mut tiles = Vec::new();
    for row in 0..page.rows {
        for col in 0..page.cols {
            let number = (row * page.cols + col + 1) as i32;
            let Some(cell) = page.cell(row, col) else {
                ids.push(None);
                tiles.push(Tile {
                    number,
                    selected: sel.empty == Some((snap.page, row, col)),
                    state: CartState::Empty,
                    ..Default::default()
                });
                continue;
            };
            let cart = &cell.cart;
            ids.push(Some(cart.id));
            tiles.push(tile(core, snap, cart, number, sel.cart == Some(cart.id)));
        }
    }
    PageView {
        rows: page.rows,
        cols: page.cols,
        ids,
        tiles,
    }
}

fn tile(core: &Core, snap: &Snapshot, cart: &Cart, number: i32, selected: bool) -> Tile {
    let rt = snap.runtime.get(&cart.id).copied().unwrap_or_default();
    let mut bg = parse_rgb(cart.color.as_deref()).unwrap_or(DEFAULT_CART_COLOR);
    if matches!(rt.state, Cs::Missing | Cs::Error | Cs::Empty) {
        bg = bg.map(|v| (v as f32 * 0.35).round() as u8);
    }
    let text = tile_text(cart, &snap.project, &rt);
    let rerouted = cart.control.is_none() && core.is_rerouted(&cart.output);
    Tile {
        has_cart: true,
        number,
        label: display_label(cart, &snap.project).into(),
        bg: rgb(bg),
        fg: text_color(bg),
        group: group_label(cart, &snap.project).unwrap_or_default().into(),
        triggers: trigger_names(cart, &snap.project).join("  ").into(),
        output: text.output.into(),
        rerouted,
        // "⟲" has no glyph in the default fonts Slint finds; "↻" does.
        flags: text.flags.join(" ").replace('⟲', "↻").into(),
        time: text.time.into(),
        // Quantised, so a tile is redrawn only when its bar visibly moves.
        progress: (rt.progress * 1000.0).round() / 1000.0,
        ending: !rt.looping && rt.remaining_s < snap.project.settings.ending_warning_s,
        state: match rt.state {
            Cs::Empty => CartState::Empty,
            Cs::Loading => CartState::Loading,
            Cs::Missing => CartState::Missing,
            Cs::Error => CartState::Error,
            Cs::Ready => CartState::Ready,
            Cs::Playing => CartState::Playing,
            Cs::Stopping => CartState::Stopping,
        },
        selected,
        learning: snap.learning.is_some_and(|l| l.cart == cart.id),
        tip: core
            .reroute_text(&cart.output)
            .filter(|_| rerouted)
            .unwrap_or_default()
            .into(),
    }
}

/// Session Settings ▸ Display as a label style. "Helvetica Bold" is the Helvetica
/// family at bold weight (the names offered by the egui app's typeface list).
pub fn label_style(d: &Display) -> LabelStyle {
    let (font, weight) = match d.font.as_deref() {
        None => (String::new(), 400),
        Some(f) => {
            if let Some(base) = f.strip_suffix(" Demi Bold") {
                (base.to_string(), 600)
            } else if let Some(base) = f.strip_suffix(" Bold") {
                (base.to_string(), 700)
            } else {
                (f.to_string(), 400)
            }
        }
    };
    LabelStyle {
        font: font.into(),
        weight,
        size: d.label_size,
        small: d.small_size,
        h_align: match d.h_align {
            HAlign::Left => 0,
            HAlign::Center => 1,
            HAlign::Right => 2,
        },
        v_align: match d.v_align {
            VAlign::Top => 0,
            VAlign::Center => 1,
            VAlign::Bottom => 2,
        },
    }
}

pub fn page_tabs(snap: &Snapshot) -> Vec<PageTab> {
    snap.project
        .pages
        .iter()
        .map(|p| PageTab {
            name: p.name.as_str().into(),
            playing: p.cells.iter().any(|c| {
                snap.runtime
                    .get(&c.cart.id)
                    .is_some_and(|r| matches!(r.state, Cs::Playing | Cs::Stopping))
            }),
        })
        .collect()
}

pub fn status(core: &Core, snap: &Snapshot) -> Vec<StatusItem> {
    core.status_items(snap)
        .into_iter()
        .map(|i| StatusItem {
            text: i.text.into(),
            error: i.level == Level::Error,
        })
        .collect()
}

fn strings(v: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

fn fade_text(ms: u32) -> String {
    if ms == 0 {
        "zero".into()
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

/// The fade-out choices for a cart: the session default, the presets, and the cart's
/// own length if it is not a preset. Returns the labels and the current index.
pub fn fade_choices(cart: &Cart, default_ms: u32) -> (Vec<String>, Vec<Option<u32>>, usize) {
    let mut labels = vec![format!("Default ({})", fade_text(default_ms))];
    let mut values = vec![None];
    for ms in FADE_PRESETS_MS {
        labels.push(fade_text(ms));
        values.push(Some(ms));
    }
    let current = cart.fade_out.map(|f| f.ms);
    if let Some(ms) = current
        && !FADE_PRESETS_MS.contains(&ms)
    {
        labels.push(fade_text(ms));
        values.push(Some(ms));
    }
    let index = values.iter().position(|v| *v == current).unwrap_or(0);
    (labels, values, index)
}

pub fn quick_edit(core: &Core, snap: &Snapshot, cart: &Cart) -> QuickEdit {
    let control = cart.control;
    let button = snap
        .project
        .cart_position(cart.id)
        .and_then(|(p, r, c)| snap.project.pages.get(p).map(|pg| r * pg.cols + c + 1))
        .unwrap_or(0);
    let rgb = parse_rgb(cart.color.as_deref()).unwrap_or(DEFAULT_CART_COLOR);
    let outputs = core.output_labels(&cart.output);
    let (fades, _, fade) = fade_choices(cart, snap.project.settings.default_fade_out_ms);
    let mut groups = vec!["none".to_string()];
    groups.extend(
        snap.project
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| g.label(i)),
    );
    let group = cart
        .group
        .and_then(|g| snap.project.groups.iter().position(|x| x.id == g))
        .map_or(0, |i| i + 1);
    QuickEdit {
        title: match control {
            Some(k) => format!("{} - Button {button}", k.name()),
            None => format!("Quick edit - Button {button}"),
        }
        .into(),
        control: control.is_some(),
        show_group: control.is_none() || control == Some(bubble_project::Control::StopGroup),
        label: cart.label.as_str().into(),
        label_hint: display_label(&Cart { label: String::new(), ..cart.clone() }, &snap.project)
            .into(),
        swatches: ModelRc::new(VecModel::from(
            SWATCHES.iter().map(|c| self::rgb(*c)).collect::<Vec<_>>(),
        )),
        color: SWATCHES.iter().position(|c| *c == rgb).map_or(-1, |i| i as i32),
        hex: cart.color.clone().unwrap_or_default().into(),
        gain: cart.gain_db,
        speed: cart.speed.max(0.01).log2(),
        output: bubble_project::output_index(&cart.output).unwrap_or(0) as i32,
        outputs: strings(outputs),
        fades: strings(fades),
        fade: fade as i32,
        groups: strings(groups),
        group: group as i32,
        triggers: trigger_names(cart, &snap.project).join(", ").into(),
        looping: cart.looping,
        hold: cart.hold_to_play,
        muted: cart.muted,
        next: cart.play_next.is_some(),
        stop_others: cart.on_play.stop != StopScope::None,
        can_paste: core.clipboard.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse() {
        assert_eq!(parse_rgb(Some("#3A6EA5")), Some(DEFAULT_CART_COLOR));
        assert_eq!(parse_rgb(Some("3a6ea5")), Some(DEFAULT_CART_COLOR));
        assert_eq!(parse_rgb(Some("#3A6EA")), None);
        assert_eq!(hex(DEFAULT_CART_COLOR), "#3A6EA5");
    }

    #[test]
    fn fade_choices_include_odd_lengths() {
        let mut cart = Cart::new();
        let (labels, values, i) = fade_choices(&cart, 1500);
        assert_eq!((labels[0].as_str(), i), ("Default (1.5 s)", 0));
        assert_eq!(values.len(), FADE_PRESETS_MS.len() + 1);
        cart.fade_out = Some(bubble_project::FadeSpec::ms(1234));
        let (labels, values, i) = fade_choices(&cart, 1500);
        assert_eq!(values[i], Some(1234));
        assert_eq!(labels[i], "1.2 s");
    }

    #[test]
    fn bold_names_become_weights() {
        let d = Display {
            font: Some("Helvetica Bold".into()),
            ..Default::default()
        };
        let s = label_style(&d);
        assert_eq!((s.font.as_str(), s.weight), ("Helvetica", 700));
    }
}
