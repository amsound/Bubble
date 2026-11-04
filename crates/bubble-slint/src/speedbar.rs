//! Speed Bar: one cart's playback speed, live (`ui/speedbar.slint`). Up to four are
//! open at once, one per cart, each above the main window.
//!
//! While the cart plays, the slider and 100% change its speed at once, x0 slows it to
//! a stop over its slow-down time and x1 brings it back to normal over its speed-up
//! time. While it is stopped they set the speed it starts at (its preset), and x0 and
//! x1 wait for it to play. The times are the cart's own, chosen here.

use bubble_control::CartState as Cs;
use bubble_core::{Intent, Snapshot};
use slint::{Color, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use crate::SpeedInfo;
use crate::model;

/// Most Speed Bars open at once.
pub const MAX_OPEN: usize = 4;

/// The times offered for x0 and x1 (ms); x1 also offers Instant.
const TIMES_MS: [u32; 8] = [500, 1000, 1500, 2000, 3000, 4000, 5000, 8000];

pub struct SpeedBar {
    pub win: crate::SpeedBarWindow,
    pub id: Uuid,
    /// x1 was pressed and the cart is on its way back to normal.
    returning: bool,
    /// The slider shows this until the cart's speed moves on its own.
    shown_speed: Option<f32>,
}

fn time_text(ms: u32) -> String {
    if ms == 0 {
        "Instant".into()
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

fn time_chip(ms: u32) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

impl SpeedBar {
    pub fn new(win: crate::SpeedBarWindow, id: Uuid) -> Self {
        win.set_times(ModelRc::new(VecModel::from(
            TIMES_MS
                .iter()
                .map(|ms| SharedString::from(time_chip(*ms)))
                .collect::<Vec<_>>(),
        )));
        SpeedBar {
            win,
            id,
            returning: false,
            shown_speed: None,
        }
    }

    fn playing(&self, snap: &Snapshot) -> Option<f32> {
        snap.runtime
            .get(&self.id)
            .filter(|r| matches!(r.state, Cs::Playing | Cs::Stopping))
            .map(|r| r.speed)
    }

    /// The slider moved: the live speed while playing, else the preset.
    pub fn speed(&mut self, v: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let v = if (v - 1.0).abs() < 0.02 {
            1.0
        } else {
            (v * 100.0).round() / 100.0
        };
        self.returning = false;
        self.shown_speed = Some(v);
        if self.playing(snap).is_some() {
            intents.push(Intent::LiveSpeed(self.id, v));
        } else if let Some(mut c) = snap.project.cart(self.id).cloned() {
            c.speed = v;
            intents.push(Intent::ReplaceCart(c));
        }
    }

    /// x0, x1, 100% and the time chooser.
    pub fn action(&mut self, what: &str, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let playing = self.playing(snap).is_some();
        match what {
            "reset" => {
                self.speed(1.0, snap, intents);
                self.win.set_speed(1.0);
            }
            "x0" if playing => {
                self.returning = false;
                intents.push(Intent::SlowToStop(self.id));
            }
            "x1" if playing => {
                self.returning = true;
                intents.push(Intent::SpeedNormal(self.id));
            }
            "pick-x0" | "pick-x1" => {
                let x0 = what == "pick-x0";
                self.win.set_picking(if x0 { 0 } else { 1 });
            }
            "close-pick" => self.win.set_picking(-1),
            _ => {}
        }
    }

    /// A time from the chooser: index into the times, or -1 for Instant (x1 only).
    pub fn time_chosen(&mut self, i: i32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let x0 = self.win.get_picking() == 0;
        let ms = usize::try_from(i)
            .ok()
            .and_then(|i| TIMES_MS.get(i).copied());
        let ms = match (ms, x0) {
            (Some(ms), _) => ms,
            (None, false) => 0,
            (None, true) => return,
        };
        if let Some(mut c) = snap.project.cart(self.id).cloned() {
            if x0 {
                c.slowdown_ms = ms;
            } else {
                c.speedup_ms = ms;
            }
            intents.push(Intent::ReplaceCart(c));
        }
        self.win.set_picking(-1);
    }

    /// Follows the cart. False when the cart is gone (the window closes).
    pub fn show(&mut self, snap: &Snapshot) -> bool {
        let Some(cart) = snap.project.cart(self.id).cloned() else {
            return false;
        };
        let rt = snap.runtime.get(&self.id).copied().unwrap_or_default();
        let live = self.playing(snap);
        let speed = live.unwrap_or(cart.speed);
        if self.returning && live.is_none_or(|s| s >= 0.995) {
            self.returning = false;
        }
        let state = match live {
            None => 0,
            Some(s) if s < 0.005 => 3,
            Some(_) if rt.slowed => 2,
            Some(_) if self.returning => 4,
            Some(_) => 1,
        };
        let (state_text, caption) = match state {
            0 => ("Stopped", "Cart preset".to_string()),
            2 => ("Slowing", "Slowing to 0".to_string()),
            3 => ("Halted", "Halted".to_string()),
            4 => ("Returning", "Returning to 100%".to_string()),
            _ => ("Playing", "Live".to_string()),
        };
        let rgb = model::parse_rgb(cart.color.as_deref()).unwrap_or(model::DEFAULT_CART_COLOR);
        let fg = model::text_color(rgb);
        let number = snap
            .project
            .cart_position(self.id)
            .map_or(0, |(p, r, c)| snap.project.cell_number(p, r, c));
        let info = SpeedInfo {
            number: number.to_string().into(),
            label: model::piped(&bubble_control::display_label(&cart, &snap.project))
                .replace('|', " ")
                .into(),
            colour: Color::from_rgb_u8(rgb[0], rgb[1], rgb[2]),
            number_colour: fg,
            state,
            state_text: state_text.into(),
            percent: format!("{:.0}%", speed * 100.0).into(),
            caption: caption.into(),
            x0_time: time_text(cart.slowdown_ms).into(),
            x1_time: time_text(cart.speedup_ms).into(),
        };
        if self.win.get_info() != info {
            self.win.set_info(info);
        }
        // The slider follows the cart, except just after it was moved here.
        if self.shown_speed.is_none_or(|v| (v - speed).abs() < 0.005) {
            self.shown_speed = None;
            if (self.win.get_speed() - speed).abs() > 0.0005 {
                self.win.set_speed(speed);
            }
        }
        let picking = self.win.get_picking();
        let current = if picking == 0 {
            cart.slowdown_ms
        } else {
            cart.speedup_ms
        };
        let chosen = match TIMES_MS.iter().position(|ms| *ms == current) {
            Some(i) => i as i32,
            None if current == 0 => -1,
            None => -2,
        };
        if self.win.get_chosen_time() != chosen {
            self.win.set_chosen_time(chosen);
        }
        let title: SharedString = if cfg!(target_os = "macos") {
            format!("Speed Bar – Cart {number}")
        } else {
            format!("Speed bar – Cart {number}")
        }
        .into();
        if self.win.get_window_title() != title {
            self.win.set_window_title(title);
        }
        true
    }
}
