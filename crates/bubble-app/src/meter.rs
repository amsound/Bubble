//! Peak programme meter (PPM) for a cart's output, dBFS scale.
//!
//! Ballistics follow the IEC 60268-10 Type I / BBC style used in broadcast PPMs:
//! effectively instantaneous rise (the engine reports true sample peaks per audio
//! block) and a slow fall of 20 dB in 1.7 s. Colours as requested: green below
//! −18 dBFS, orange from −18 to −10 dBFS, red above −10 dBFS.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

pub const FLOOR_DB: f32 = -60.0;
/// Fall rate: 20 dB in 1.7 s.
const FALL_DB_PER_S: f32 = 20.0 / 1.7;
/// Peak-hold markers fall after this long.
const HOLD_S: f32 = 1.5;

#[derive(Clone, Copy, Debug)]
pub struct Ppm {
    level: [f32; 2],
    hold: [f32; 2],
    hold_age: [f32; 2],
}

impl Default for Ppm {
    fn default() -> Self {
        Ppm {
            level: [FLOOR_DB; 2],
            hold: [FLOOR_DB; 2],
            hold_age: [0.0; 2],
        }
    }
}

pub fn to_db(lin: f32) -> f32 {
    if lin <= 1e-6 {
        FLOOR_DB
    } else {
        (20.0 * lin.log10()).max(FLOOR_DB)
    }
}

impl Ppm {
    /// Feeds the linear peaks seen since the last frame, `dt` seconds ago.
    pub fn update(&mut self, peak: (f32, f32), dt: f32) {
        let dt = dt.clamp(0.0, 0.5);
        for (i, p) in [peak.0, peak.1].into_iter().enumerate() {
            let db = to_db(p);
            let fallen = self.level[i] - FALL_DB_PER_S * dt;
            self.level[i] = db.max(fallen).max(FLOOR_DB);
            self.hold_age[i] += dt;
            if self.level[i] >= self.hold[i] {
                self.hold[i] = self.level[i];
                self.hold_age[i] = 0.0;
            } else if self.hold_age[i] > HOLD_S {
                self.hold[i] = (self.hold[i] - FALL_DB_PER_S * dt).max(self.level[i]);
            }
        }
    }

    #[cfg(test)]
    pub fn level(&self) -> [f32; 2] {
        self.level
    }

    pub fn is_idle(&self) -> bool {
        self.level.iter().all(|l| *l <= FLOOR_DB + 0.01)
            && self.hold.iter().all(|h| *h <= FLOOR_DB + 0.01)
    }
}

/// Green up to and including −18 dBFS, orange to −10, red above.
pub fn colour_for(db: f32) -> Color32 {
    if db > -10.0 {
        Color32::from_rgb(0xE5, 0x39, 0x35) // red
    } else if db > -18.0 {
        Color32::from_rgb(0xFB, 0x8C, 0x00) // orange
    } else {
        Color32::from_rgb(0x43, 0xA0, 0x47) // green
    }
}

const TICKS: &[f32] = &[0.0, -10.0, -18.0, -60.0];

/// Scale position 0 (bottom, −60) … 1 (top, 0 dBFS). The working range gets the
/// room: −18 sits halfway up and −10 three quarters up (UK PPM 4 and PPM 6).
pub fn scale_pos(db: f32) -> f32 {
    let d = db.clamp(FLOOR_DB, 0.0);
    if d <= -18.0 {
        (d - FLOOR_DB) / (-18.0 - FLOOR_DB) * 0.5
    } else if d <= -10.0 {
        0.5 + (d + 18.0) / 8.0 * 0.25
    } else {
        0.75 + (d + 10.0) / 10.0 * 0.25
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
        let lvl = ppm.level[i];
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
        let hold = ppm.hold[i];
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
    fn rises_instantly_and_falls_20_db_in_1_7_s() {
        let mut m = Ppm::default();
        m.update((0.5, 0.25), 0.02);
        assert!((m.level()[0] - (-6.02)).abs() < 0.01);
        assert!((m.level()[1] - (-12.04)).abs() < 0.01);
        for _ in 0..17 {
            m.update((0.0, 0.0), 0.1);
        }
        assert!((m.level()[0] - (-26.02)).abs() < 0.05, "{}", m.level()[0]);
    }

    #[test]
    fn colour_zones() {
        assert_eq!(colour_for(-30.0), colour_for(-18.0), "-18 is still green");
        assert_eq!(colour_for(-17.9), colour_for(-10.0));
        assert_eq!(colour_for(-9.0), colour_for(0.0));
        assert_ne!(colour_for(-20.0), colour_for(-15.0));
        assert_ne!(colour_for(-15.0), colour_for(-5.0));
    }

    #[test]
    fn scale_gives_the_working_range_room() {
        assert_eq!(scale_pos(-60.0), 0.0);
        assert!((scale_pos(-18.0) - 0.5).abs() < 1e-6);
        assert!((scale_pos(-10.0) - 0.75).abs() < 1e-6);
        assert!((scale_pos(0.0) - 1.0).abs() < 1e-6);
        assert!(scale_pos(-30.0) < scale_pos(-18.0));
    }
}
