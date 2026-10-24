//! Peak programme meter (PPM) for a cart's output, drawn in egui; the ballistics and
//! scale are `bubble_core::meter`.

pub use bubble_core::meter::Ppm;
use bubble_core::meter::{FLOOR_DB, TICKS, scale_pos, zone};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

/// Green up to and including −18 dBFS, orange to −10, red above.
pub fn colour_for(db: f32) -> Color32 {
    match zone(db) {
        2 => Color32::from_rgb(0xE5, 0x39, 0x35), // red
        1 => Color32::from_rgb(0xFB, 0x8C, 0x00), // orange
        _ => Color32::from_rgb(0x43, 0xA0, 0x47), // green
    }
}

/// Draws a vertical two-channel meter filling `height`.
pub fn ppm_ui(ui: &mut egui::Ui, ppm: &Ppm, height: f32) {
    let width = 64.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let dark = ui.visuals().dark_mode;
    let (bg, track, text) = if dark {
        (
            Color32::from_gray(24),
            Color32::from_gray(44),
            Color32::from_gray(160),
        )
    } else {
        (
            Color32::from_gray(238),
            Color32::from_gray(214),
            Color32::from_gray(90),
        )
    };
    painter.rect_filled(rect, 3.0, bg);
    let top = rect.top() + 14.0;
    let bottom = rect.bottom() - 6.0;
    let y_of = |db: f32| bottom - scale_pos(db) * (bottom - top);
    let scale_x = rect.left() + 26.0;
    for t in TICKS {
        let y = y_of(*t);
        painter.line_segment(
            [Pos2::new(scale_x - 3.0, y), Pos2::new(scale_x, y)],
            Stroke::new(1.0, text),
        );
        painter.text(
            Pos2::new(scale_x - 5.0, y),
            Align2::RIGHT_CENTER,
            format!("{}", *t as i32),
            FontId::monospace(9.0),
            text,
        );
    }
    let bar_w = 14.0;
    for (i, label) in ["L", "R"].iter().enumerate() {
        let x0 = scale_x + 4.0 + i as f32 * (bar_w + 4.0);
        let bar = Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x0 + bar_w, bottom));
        painter.rect_filled(bar, 1.0, track);
        // Fill in colour zones up to the level.
        let lvl = ppm.level()[i];
        for (lo, hi) in [(FLOOR_DB, -18.0), (-18.0, -10.0), (-10.0, 0.0)] {
            if lvl <= lo {
                continue;
            }
            let seg = Rect::from_min_max(
                Pos2::new(x0, y_of(lvl.min(hi))),
                Pos2::new(x0 + bar_w, y_of(lo)),
            );
            painter.rect_filled(seg, 0.0, colour_for(hi));
        }
        let hold = ppm.hold()[i];
        if hold > FLOOR_DB + 0.5 {
            let y = y_of(hold);
            painter.line_segment(
                [Pos2::new(x0, y), Pos2::new(x0 + bar_w, y)],
                Stroke::new(2.0, colour_for(hold)),
            );
        }
        painter.text(
            Pos2::new(x0 + bar_w / 2.0, rect.top() + 2.0),
            Align2::CENTER_TOP,
            *label,
            FontId::proportional(10.0),
            text,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_zones() {
        assert_eq!(colour_for(-30.0), colour_for(-18.0), "-18 is still green");
        assert_eq!(colour_for(-17.9), colour_for(-10.0));
        assert_eq!(colour_for(-9.0), colour_for(0.0));
        assert_ne!(colour_for(-20.0), colour_for(-15.0));
        assert_ne!(colour_for(-15.0), colour_for(-5.0));
    }
}
