//! Building blocks for Cart Setup's form, so every row lines up: a fixed label
//! column, sliders of one track length with a centre tick, and value boxes of one
//! width.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};

/// Width of the value box at the end of a slider row.
pub(super) const VALUE_W: f32 = 70.0;
/// Gap between a slider's track and its value box.
const GAP: f32 = 10.0;

/// A horizontal slider whose track fills `width`, with a tick at `centre` and the
/// fill drawn from the centre to the knob. `log` maps the track logarithmically
/// (speed). Double-click returns to the centre. Values outside the range are kept;
/// the knob pins at the end. Returns true when changed.
pub(super) fn centred_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    (min, max): (f32, f32),
    centre: f32,
    log: bool,
    width: f32,
) -> bool {
    let to_t = |v: f32| -> f32 {
        let v = v.clamp(min, max);
        if log {
            (v.ln() - min.ln()) / (max.ln() - min.ln())
        } else {
            (v - min) / (max - min)
        }
    };
    let from_t = |t: f32| -> f32 {
        let t = t.clamp(0.0, 1.0);
        if log {
            (min.ln() + t * (max.ln() - min.ln())).exp()
        } else {
            min + t * (max - min)
        }
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click_and_drag());
    let pad = 8.0; // keep the knob inside the row
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + pad, rect.center().y - 2.0),
        Pos2::new(rect.right() - pad, rect.center().y + 2.0),
    );
    let x_of = |t: f32| track.left() + t * track.width();
    let mut changed = false;
    if resp.double_clicked() {
        *value = centre;
        changed = true;
    } else if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let t = (p.x - track.left()) / track.width();
        let mut v = from_t(t);
        // Snap to the centre when close, so 0 dB / centre / 100 % are easy to hit.
        if (x_of(to_t(centre)) - p.x).abs() < 4.0 {
            v = centre;
        }
        if v != *value {
            *value = v;
            changed = true;
        }
    }
    let vis = ui.visuals();
    let accent = vis.selection.bg_fill;
    let painter = ui.painter();
    painter.rect_filled(track, 2.0, vis.widgets.inactive.bg_fill);
    let (tc, tv) = (x_of(to_t(centre)), x_of(to_t(*value)));
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(tc.min(tv), track.top()),
            Pos2::new(tc.max(tv), track.bottom()),
        ),
        2.0,
        accent,
    );
    painter.line_segment(
        [
            Pos2::new(tc, track.top() - 5.0),
            Pos2::new(tc, track.bottom() + 5.0),
        ],
        Stroke::new(1.0, vis.widgets.noninteractive.fg_stroke.color),
    );
    let knob = Pos2::new(tv, track.center().y);
    let hot = resp.hovered() || resp.dragged();
    painter.circle_filled(
        knob,
        7.5,
        vis.widgets.active.weak_bg_fill.gamma_multiply(1.0),
    );
    painter.circle_filled(
        knob,
        7.0,
        if hot {
            Color32::WHITE
        } else {
            vis.extreme_bg_color
        },
    );
    painter.circle_stroke(
        knob,
        7.0,
        Stroke::new(1.0, vis.widgets.inactive.fg_stroke.color),
    );
    changed
}

/// A slider row: track filling the space left of a fixed-width value box.
#[allow(clippy::too_many_arguments)]
pub(super) fn slider_row(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: (f32, f32),
    centre: f32,
    log: bool,
    format: impl Fn(f64) -> String,
    parse: impl Fn(&str) -> Option<f64>,
    speed: f64,
) -> bool {
    let w = (ui.available_width() - VALUE_W - GAP).max(80.0);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        changed |= centred_slider(ui, value, range, centre, log, w);
        let mut v = *value as f64;
        let r = ui.add_sized(
            [VALUE_W, 22.0],
            egui::DragValue::new(&mut v)
                .speed(speed)
                .range(range.0 as f64..=range.1 as f64)
                .custom_formatter(|x, _| format(x))
                .custom_parser(|s| parse(s)),
        );
        if r.changed() {
            *value = v as f32;
            changed = true;
        }
    });
    changed
}

pub(super) fn pan_text(p: f64) -> String {
    let n = (p * 100.0).round() as i32;
    match n {
        0 => "C".into(),
        n if n < 0 => format!("L {}", -n),
        n => format!("R {n}"),
    }
}

pub(super) fn parse_pan(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_uppercase();
    if t == "C" || t.is_empty() {
        return Some(0.0);
    }
    let (sign, rest) = match t.chars().next()? {
        'L' => (-1.0, &t[1..]),
        'R' => (1.0, &t[1..]),
        _ => (1.0, t.as_str()),
    };
    rest.trim().parse::<f64>().ok().map(|v| sign * v / 100.0)
}

pub(super) fn parse_db(s: &str) -> Option<f64> {
    s.trim()
        .trim_end_matches("dB")
        .trim_end_matches("db")
        .trim()
        .replace('−', "-")
        .parse()
        .ok()
}

/// A large time readout of exactly `width`.
pub(super) fn readout(ui: &mut egui::Ui, text: &str, colour: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
    let dark = ui.visuals().dark_mode;
    ui.painter().rect_filled(
        rect,
        6.0,
        if dark {
            Color32::from_gray(16)
        } else {
            Color32::from_gray(28)
        },
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(19.0),
        colour,
    );
}
