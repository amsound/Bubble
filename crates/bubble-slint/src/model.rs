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
    /// The cell audio is being copied into.
    pub copying: Option<(usize, u16, u16)>,
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
            let copying = sel.copying == Some((snap.page, row, col));
            let Some(cell) = page.cell(row, col) else {
                ids.push(None);
                tiles.push(Tile {
                    number,
                    selected: sel.empty == Some((snap.page, row, col)),
                    state: CartState::Empty,
                    copying,
                    ..Default::default()
                });
                continue;
            };
            let cart = &cell.cart;
            ids.push(Some(cart.id));
            let mut t = tile(core, snap, cart, number, sel.cart == Some(cart.id));
            t.copying = copying;
            tiles.push(t);
        }
    }
    PageView {
        rows: page.rows,
        cols: page.cols,
        ids,
        tiles,
    }
}

/// Path commands (16 × 16 grid) for the tile's icon row; the same icons as the design.
pub mod icon {
    pub const LOOP: &str =
        "M2.5 7.5a5 5 0 0 1 8.6-3.5M13.5 8.5a5 5 0 0 1-8.6 3.5M11.5 1.5v2.8H8.7M4.5 14.5v-2.8h2.8";
    pub const IN: &str = "M6 3H4v10h2M8.5 8h5M11 5.5L13.5 8L11 10.5";
    pub const OUT: &str = "M10 3h2v10h-2M2.5 8h5M5 5.5L2.5 8L5 10.5";
    pub const IN_OUT: &str = "M5.5 3h-2v10h2M10.5 3h2v10h-2";
    pub const STOP_ALL: &str = "M4.5 4.5h7v7h-7z";
    pub const NEXT: &str = "M2.5 8h8M7.5 5l3 3-3 3M13 4v8";
    pub const MUTE: &str = "M2 6h2.5L8 3v10L4.5 10H2zM10.5 6l4 4M14.5 6l-4 4";
    pub const HOLD: &str = "M6 14V6.5a1.5 1.5 0 0 1 3 0V9h1.5a2 2 0 0 1 2 2v3";
    pub const MIDI: &str = "M8 2a6 6 0 1 0 0.01 0zM5 8h1.2M7.4 5.5h1.2M9.8 8h1.2M6 10.6h1M9 10.6h1";
    pub const CHANGED: &str = "M8 2.5l6 11H2zM8 6.5v3M8 11.2v.8";
}

/// One shared model per distinct icon row. Slint compares models by identity, so a
/// new model each refresh would make every tile look changed and redraw.
fn icon_model(icons: Vec<SharedString>) -> ModelRc<SharedString> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<Vec<SharedString>, ModelRc<SharedString>>> =
            Default::default();
    }
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(icons.clone())
            .or_insert_with(|| ModelRc::new(VecModel::from(icons)))
            .clone()
    })
}

fn tile(core: &Core, snap: &Snapshot, cart: &Cart, number: i32, selected: bool) -> Tile {
    let rt = snap.runtime.get(&cart.id).copied().unwrap_or_default();
    let mut bg = parse_rgb(cart.color.as_deref()).unwrap_or(DEFAULT_CART_COLOR);
    if matches!(rt.state, Cs::Missing | Cs::Error | Cs::Empty) {
        bg = bg.map(|v| (v as f32 * 0.35).round() as u8);
    }
    let text = tile_text(cart, &snap.project, &rt);
    let rerouted = cart.control.is_none() && core.is_rerouted(&cart.output);
    let control = cart.control.is_some();
    // Key triggers on the keycap; a MIDI trigger is an icon in the row.
    let (midi, keys): (Vec<_>, Vec<_>) = snap
        .project
        .bindings
        .iter()
        .filter(|b| b.targets.contains(&cart.id))
        .partition(|b| matches!(b.source, bubble_project::BindingSource::Midi { .. }));
    let keys: Vec<String> = keys
        .iter()
        .map(|b| bubble_control::describe(&b.source))
        .collect();
    let mut icons: Vec<SharedString> = Vec::new();
    if !control {
        match bubble_core::view::trim_flags(cart, &snap.project) {
            (true, true) => icons.push(icon::IN_OUT.into()),
            (true, false) => icons.push(icon::IN.into()),
            (false, true) => icons.push(icon::OUT.into()),
            _ => {}
        }
        if cart.looping {
            icons.push(icon::LOOP.into());
        }
        if cart.on_play.stop != StopScope::None {
            icons.push(icon::STOP_ALL.into());
        }
        if cart.play_next.is_some() {
            icons.push(icon::NEXT.into());
        }
        if cart.muted {
            icons.push(icon::MUTE.into());
        }
        if cart.hold_to_play {
            icons.push(icon::HOLD.into());
        }
        if rt.changed {
            icons.push(icon::CHANGED.into());
        }
    }
    if !midi.is_empty() {
        icons.push(icon::MIDI.into());
    }
    let playing = matches!(rt.state, Cs::Playing | Cs::Stopping);
    let speed = if playing { rt.speed } else { cart.speed };
    Tile {
        has_cart: true,
        number,
        label: display_label(cart, &snap.project).replace('|', "\n").into(),
        bg: rgb(bg),
        fg: text_color(bg),
        group: group_label(cart, &snap.project).unwrap_or_default().into(),
        triggers: keys.join("  ").into(),
        output: if control {
            String::new()
        } else {
            cart.output.clone()
        }
        .into(),
        rerouted,
        icons: icon_model(icons),
        speed: if !control && (speed - 1.0).abs() > 0.005 {
            format!("×{speed:.2}")
        } else {
            String::new()
        }
        .into(),
        time: if rt.state == Cs::Loading {
            String::new()
        } else {
            text.time
        }
        .into(),
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
        control,
        tip: core
            .reroute_text(&cart.output)
            .filter(|_| rerouted)
            .unwrap_or_default()
            .into(),
        copying: false,
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

/// The status line, plus `info` (audio being prepared) first.
pub fn status(core: &Core, snap: &Snapshot, info: Option<String>) -> Vec<StatusItem> {
    let info = info.map(|text| StatusItem {
        text: text.into(),
        info: true,
        ..Default::default()
    });
    info.into_iter()
        .chain(core.status_items(snap).into_iter().map(|i| StatusItem {
            text: i.text.into(),
            error: i.level == Level::Error,
            info: false,
        }))
        .collect()
}

fn strings(v: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

pub fn fade_text(ms: u32) -> String {
    if ms == 0 {
        "None".into()
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
    let mut groups = vec!["None".to_string()];
    groups.extend(
        snap.project
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| g.display_name(i)),
    );
    let group = cart
        .group
        .and_then(|g| snap.project.groups.iter().position(|x| x.id == g))
        .map_or(0, |i| i + 1);
    QuickEdit {
        title: match control {
            Some(k) => format!("{} - Button {button}", k.name()),
            None => format!("Button {button}"),
        }
        .into(),
        control: control.is_some(),
        show_group: control.is_none() || control == Some(bubble_project::Control::StopGroup),
        label: cart.label.as_str().into(),
        swatches: ModelRc::new(VecModel::from(
            SWATCHES.iter().map(|c| self::rgb(*c)).collect::<Vec<_>>(),
        )),
        color: SWATCHES
            .iter()
            .position(|c| *c == rgb)
            .map_or(-1, |i| i as i32),
        hex: cart.color.clone().unwrap_or_default().into(),
        gain: cart.gain_db,
        output: bubble_project::output_index(&cart.output).unwrap_or(0) as i32,
        outputs: strings(outputs),
        fades: strings(fades),
        fade: fade as i32,
        groups: strings(groups),
        group: group as i32,
        hotkey: trigger_names(cart, &snap.project).join("  ").into(),
        cart_color: self::rgb(rgb),
        looping: cart.looping,
        next: cart.play_next.is_some(),
        stop_all: cart.on_play.stop != StopScope::None,
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
