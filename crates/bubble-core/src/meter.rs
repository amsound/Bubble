//! Peak programme meter (PPM) ballistics and scale for a cart's output, in dBFS; each
//! interface draws it its own way.
//!
//! Instantaneous rise (the engine reports true sample peaks per audio block) and a
//! quick fall of 20 dB in 0.6 s, so the meter follows the audio closely; the peak
//! hold stays a second. Zones: green to −18 dBFS, orange to −10 dBFS, red above.

pub const FLOOR_DB: f32 = -60.0;
/// Fall rate: 20 dB in 0.6 s.
const FALL_DB_PER_S: f32 = 20.0 / 0.6;
/// Peak-hold markers fall after this long.
const HOLD_S: f32 = 1.0;
/// Scale marks (dBFS).
pub const TICKS: &[f32] = &[0.0, -10.0, -18.0, -60.0];

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

    /// Left and right level (dBFS).
    pub fn level(&self) -> [f32; 2] {
        self.level
    }

    /// Left and right peak-hold (dBFS).
    pub fn hold(&self) -> [f32; 2] {
        self.hold
    }

    pub fn is_idle(&self) -> bool {
        self.level.iter().all(|l| *l <= FLOOR_DB + 0.01)
            && self.hold.iter().all(|h| *h <= FLOOR_DB + 0.01)
    }
}

/// The zone a level is in: 0 green (to −18 dBFS), 1 orange (to −10), 2 red.
pub fn zone(db: f32) -> u8 {
    if db > -10.0 {
        2
    } else if db > -18.0 {
        1
    } else {
        0
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rises_instantly_and_falls_20_db_in_0_6_s() {
        let mut m = Ppm::default();
        m.update((0.5, 0.25), 0.02);
        assert!((m.level()[0] - (-6.02)).abs() < 0.01);
        assert!((m.level()[1] - (-12.04)).abs() < 0.01);
        for _ in 0..6 {
            m.update((0.0, 0.0), 0.1);
        }
        assert!((m.level()[0] - (-26.02)).abs() < 0.05, "{}", m.level()[0]);
    }

    #[test]
    fn zones() {
        assert_eq!(zone(-30.0), zone(-18.0), "-18 is still green");
        assert_eq!(zone(-17.9), zone(-10.0));
        assert_eq!(zone(-9.0), zone(0.0));
        assert_ne!(zone(-20.0), zone(-15.0));
        assert_ne!(zone(-15.0), zone(-5.0));
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
