//! Cart Setup: one cart in detail, in its own window (`ui/cartsetup.slint`).
//!
//! Edits go to a draft: OK replaces the cart with it (one undo step), Cancel drops it,
//! and closing the window with unkept edits asks first (app.rs). While the cart is
//! auditioned, what you hear follows the draft at once (gain, mute, speed, loop,
//! trims). The waveform is drawn from paths made here for the part on screen, and every
//! press, drag, scroll, pinch and key in it comes here: only the IN and OUT handles
//! (or their tabs at the edge, when off screen) move In and Out; a click places the
//! cursor, a double-click plays from there, a drag elsewhere scrolls.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bubble_control::{AssetState, CartState as Cs, describe};
use bubble_core::app::Core;
use bubble_core::meter::{Ppm, scale_pos, zone};
use bubble_core::{Intent, Snapshot};
use bubble_engine::Overview;
use bubble_project::{BindingSource, Cart, FadeSpec, Retrigger, StopScope, Trim};
use slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use crate::model;
use crate::{PopupRow, SetupInfo, WaveMark};

/// Pixels either side of a handle (or the playhead) that grab it.
const GRAB: f32 = 12.0;
/// The IN and OUT flags at the top of their lines also grab them.
const FLAG_H: f32 = 22.0;
const FLAG_W: f32 = 34.0;
/// Width of the edge strip that scrolls while a handle is dragged into it.
const EDGE: f32 = 32.0;
/// Shortest In–Out (seconds).
const MIN_LEN: f64 = 0.001;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Handle {
    In,
    Out,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Handle(Handle),
    /// The playhead while playing: dragging it moves the audition.
    Playhead,
    /// Dragging the waveform itself scrolls it.
    Pan {
        x: f32,
        start: f64,
    },
}

/// A choice in one of the pop-up buttons' native menus.
#[derive(Clone, Debug)]
enum Pick {
    Output(usize),
    FadeIn(Option<u32>),
    FadeOut(Option<u32>),
    SlowDown(u32),
    SpeedUp(u32),
    Retrigger(Option<Retrigger>),
    Group(Option<Uuid>),
}

pub struct Setup {
    pub win: crate::CartSetupWindow,
    pub id: Uuid,
    pub draft: Cart,
    /// The cart as it is in the session (what OK compares against).
    base: Cart,
    // The view: `start` and `span` in seconds of the file.
    start: f64,
    span: f64,
    dur: f64,
    rate: f64,
    frames: u64,
    channels: u16,
    overview: Option<Arc<Overview>>,
    cursor: Option<f64>,
    last: Handle,
    drag: Option<Drag>,
    press: f32,
    moved: bool,
    /// Pointer x in the wave area, while it is over or dragging in it.
    pointer: Option<f32>,
    ppm: Ppm,
    last_meter: Instant,
    wave_sig: (u64, u64, u32, u32, usize),
    popup: Vec<Pick>,
    /// Sliders and text to set from here on the next show (opening, a reset).
    push: bool,
    /// Two-finger pinch on a touch screen: the fingers and the last distance.
    touches: Vec<(u64, f64, f64)>,
    pinch: Option<f64>,
    /// The playhead was grabbed while playing: it carries on when let go.
    resume: bool,
    /// Scrubbing: when the held playhead last moved, and whether it is sounding.
    scrub_at: Option<Instant>,
    sounding: bool,
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:05.2}", m as u32, s - m * 60.0)
}

fn seconds_text(ms: u32) -> String {
    if ms == 0 {
        "Instant".into()
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

pub(crate) fn retrigger_text(r: Retrigger) -> &'static str {
    match r {
        Retrigger::Restart => "Restart",
        Retrigger::Overlap => "Overlap",
        Retrigger::ToggleStop => {
            if cfg!(target_os = "macos") {
                "Toggle Stop"
            } else {
                "Toggle stop"
            }
        }
        Retrigger::Ignore => "Ignore",
    }
}

fn pan_text(p: f32) -> String {
    if p.abs() < 0.005 {
        "Centre".into()
    } else if p < 0.0 {
        format!("L {:.0}", -p * 100.0)
    } else {
        format!("R {:.0}", p * 100.0)
    }
}

fn gain_text(db: f32) -> String {
    let v = (db * 10.0).round() / 10.0;
    if v == 0.0 {
        "0 dB".into()
    } else if v > 0.0 {
        format!("+{v:.1} dB")
    } else {
        format!("−{:.1} dB", -v)
    }
}

impl Setup {
    pub fn new(win: crate::CartSetupWindow, cart: Cart) -> Self {
        Setup {
            win,
            id: cart.id,
            draft: cart.clone(),
            base: cart,
            start: 0.0,
            span: 0.0,
            dur: 0.0,
            rate: 1.0,
            frames: 0,
            channels: 0,
            overview: None,
            cursor: None,
            last: Handle::In,
            drag: None,
            press: 0.0,
            moved: false,
            pointer: None,
            ppm: Ppm::default(),
            last_meter: Instant::now(),
            wave_sig: (0, 0, 0, 0, 0),
            popup: Vec::new(),
            push: true,
            touches: Vec::new(),
            pinch: None,
            resume: false,
            scrub_at: None,
            sounding: false,
        }
    }

    pub fn changed(&self) -> bool {
        self.draft != self.base
    }

    fn playing(&self, snap: &Snapshot) -> bool {
        snap.runtime
            .get(&self.id)
            .is_some_and(|r| matches!(r.state, Cs::Playing | Cs::Stopping))
    }

    /// Paused here (held at speed 0): read from the cart itself, so it always matches.
    fn paused(&self, snap: &Snapshot) -> bool {
        snap.runtime
            .get(&self.id)
            .is_some_and(|r| matches!(r.state, Cs::Playing | Cs::Stopping) && r.speed < 0.005)
    }

    fn trims(&self) -> (f64, f64) {
        match self.draft.trim {
            Some(t) => (
                t.in_frames as f64 / self.rate,
                t.out_frames as f64 / self.rate,
            ),
            None => (0.0, self.dur),
        }
    }

    fn set_trims(&mut self, in_s: f64, out_s: f64, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if self.dur <= 0.0 {
            return;
        }
        let in_s = in_s.clamp(0.0, (self.dur - MIN_LEN).max(0.0));
        let out_s = out_s.clamp(in_s + MIN_LEN, self.dur);
        let in_f = (in_s * self.rate).round() as u64;
        let out_f = ((out_s * self.rate).round() as u64).min(self.frames);
        self.draft.trim = if in_f == 0 && out_f >= self.frames {
            None
        } else {
            Some(Trim {
                in_frames: in_f,
                out_frames: out_f.max(in_f + 1),
            })
        };
        // Heard straight away while auditioning.
        if self.playing(snap) {
            intents.push(Intent::LiveTrim(self.id, in_s, out_s));
        }
    }

    fn move_handle(&mut self, h: Handle, t: f64, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let (a, b) = self.trims();
        match h {
            Handle::In => self.set_trims(t.min(b - MIN_LEN), b, snap, intents),
            Handle::Out => self.set_trims(a, t.max(a + MIN_LEN), snap, intents),
        }
        self.last = h;
    }

    // ---- the view ----

    fn width(&self) -> f32 {
        self.win.get_wave_width().max(1.0)
    }

    fn x_of(&self, t: f64) -> f32 {
        if self.span <= 0.0 {
            return -100.0;
        }
        ((t - self.start) / self.span * self.width() as f64) as f32
    }

    fn t_of(&self, x: f32) -> f64 {
        (self.start + x as f64 / self.width() as f64 * self.span).clamp(0.0, self.dur)
    }

    fn clamp_view(&mut self) {
        let min_span = (self.dur / 4000.0).max(0.005).min(self.dur);
        self.span = self.span.clamp(min_span, self.dur.max(min_span));
        self.start = self.start.clamp(0.0, (self.dur - self.span).max(0.0));
    }

    /// Zooms by `factor` (above 1 zooms in), keeping `anchor` (seconds) where it is.
    fn zoom(&mut self, factor: f64, anchor: f64) {
        if self.dur <= 0.0 {
            return;
        }
        let frac = (anchor - self.start) / self.span;
        self.span /= factor;
        self.clamp_view();
        self.start = anchor - frac * self.span;
        self.clamp_view();
    }

    fn zoom_fit(&mut self) {
        self.start = 0.0;
        self.span = self.dur;
    }

    fn anchor(&self) -> f64 {
        match self.pointer {
            Some(x) => self.t_of(x),
            None => self.start + self.span / 2.0,
        }
    }

    // ---- input from the window ----

    pub fn wave_down(&mut self, x: f32, y: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        self.pointer = Some(x);
        self.press = x;
        self.moved = false;
        if self.pinch.is_some() {
            return;
        }
        let w = self.width();
        let (a, b) = self.trims();
        let (in_x, out_x) = (self.x_of(a), self.x_of(b));
        let top = y < FLAG_H;
        let din = (x - in_x).abs();
        let dout = (x - out_x).abs();
        let dplay = self
            .playhead(snap)
            .map_or(f32::MAX, |t| (x - self.x_of(t)).abs());
        self.drag = Some(
            // A handle off screen is a tab pinned to its edge.
            if top && in_x < 0.0 && x < 120.0 {
                Drag::Handle(Handle::In)
            } else if top && out_x > w && x > w - 120.0 {
                Drag::Handle(Handle::Out)
            // The flags: IN's to the right of its line, OUT's to the left.
            } else if top && x >= in_x - 4.0 && x <= in_x + FLAG_W && (in_x - out_x).abs() > 1.0 {
                Drag::Handle(Handle::In)
            } else if top && x <= out_x + 4.0 && x >= out_x - FLAG_W {
                Drag::Handle(Handle::Out)
            } else if dplay <= GRAB && dplay < din.min(dout) {
                Drag::Playhead
            } else if din <= GRAB && din <= dout {
                Drag::Handle(Handle::In)
            } else if dout <= GRAB {
                Drag::Handle(Handle::Out)
            } else {
                Drag::Pan {
                    x,
                    start: self.start,
                }
            },
        );
        match self.drag {
            Some(Drag::Handle(h)) => self.last = h,
            // Holding the playhead still holds the sound; moving it plays from
            // where it is (scrubbing); letting go carries on as before.
            Some(Drag::Playhead) => {
                if !self.paused(snap) {
                    intents.push(Intent::LiveSpeedRamp(self.id, 0.0, 5));
                    self.resume = true;
                }
                self.sounding = false;
            }
            _ => {}
        }
    }

    pub fn wave_move(&mut self, x: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        self.pointer = Some(x);
        if (x - self.press).abs() > 3.0 {
            self.moved = true;
        }
        match self.drag {
            Some(Drag::Handle(h)) if self.moved => {
                let t = self.t_of(x);
                self.move_handle(h, t, snap, intents);
            }
            Some(Drag::Playhead) if self.moved && self.playing(snap) => {
                let (a, b) = self.trims();
                let t = self.t_of(x).clamp(a, (b - MIN_LEN).max(a));
                intents.push(Intent::Seek(self.id, t));
                if !self.sounding {
                    intents.push(Intent::LiveSpeedRamp(self.id, self.draft.speed, 5));
                    self.sounding = true;
                }
                self.scrub_at = Some(Instant::now());
            }
            Some(Drag::Pan { x: x0, start }) => {
                self.start = start - (x - x0) as f64 / self.width() as f64 * self.span;
                self.clamp_view();
            }
            _ => {}
        }
    }

    pub fn wave_up(&mut self, x: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        // A click that did not drag places the cursor.
        if !self.moved && matches!(self.drag, Some(Drag::Pan { .. })) {
            self.cursor = Some(self.t_of(x));
        }
        if matches!(self.drag, Some(Drag::Playhead)) && self.playing(snap) {
            let resume = std::mem::take(&mut self.resume);
            if resume && !self.sounding {
                intents.push(Intent::LiveSpeedRamp(self.id, self.draft.speed, 5));
            } else if !resume && self.sounding {
                intents.push(Intent::LiveSpeedRamp(self.id, 0.0, 5));
            }
        }
        self.sounding = false;
        self.scrub_at = None;
        self.drag = None;
    }

    /// A double-click plays from there.
    pub fn wave_double(&mut self, x: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let t = self.t_of(x);
        self.cursor = Some(t);
        self.play_from(t, snap, intents);
    }

    /// Scrolling moves through time; with ⌘ (Ctrl on Windows) it zooms.
    pub fn wave_scroll(&mut self, dx: f32, dy: f32, zoom: bool, shift: bool) {
        if self.dur <= 0.0 {
            return;
        }
        if zoom {
            let anchor = self.anchor();
            self.zoom((dy as f64 * 0.01).exp(), anchor);
            return;
        }
        let d = if shift || dx.abs() > dy.abs() { dx } else { dy };
        self.start -= d as f64 / self.width() as f64 * self.span;
        self.clamp_view();
    }

    /// Trackpad pinch (macOS): `delta` above 0 zooms in.
    pub fn pinch(&mut self, delta: f64) {
        let anchor = self.anchor();
        self.zoom(1.0 + delta, anchor);
    }

    /// Touch screens: two fingers pinch to zoom. Returns true while pinching, so the
    /// first finger's drag is ignored.
    pub fn touch(&mut self, id: u64, x: f64, y: f64, down: bool) -> bool {
        self.touches.retain(|t| t.0 != id);
        if down {
            self.touches.push((id, x, y));
        }
        if self.touches.len() == 2 {
            let (a, b) = (self.touches[0], self.touches[1]);
            let d = ((a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt().max(1.0);
            if let Some(prev) = self.pinch {
                let anchor = self.start + self.span / 2.0;
                self.zoom(d / prev, anchor);
            }
            self.pinch = Some(d);
            self.drag = None;
            true
        } else {
            self.pinch = None;
            false
        }
    }

    /// While a handle is dragged into either edge, the waveform scrolls that way.
    pub fn auto_scroll(&mut self, snap: &Snapshot, intents: &mut Vec<Intent>) -> bool {
        // A scrubbed playhead held still goes quiet again.
        if matches!(self.drag, Some(Drag::Playhead))
            && self.sounding
            && self
                .scrub_at
                .is_some_and(|t| t.elapsed() > Duration::from_millis(120))
        {
            intents.push(Intent::LiveSpeedRamp(self.id, 0.0, 5));
            self.sounding = false;
        }
        let (Some(Drag::Handle(h)), Some(x)) = (self.drag, self.pointer) else {
            return false;
        };
        let w = self.width();
        let push = if x < EDGE {
            -((EDGE - x) / EDGE).min(1.5)
        } else if x > w - EDGE {
            ((x - (w - EDGE)) / EDGE).min(1.5)
        } else {
            return false;
        };
        let before = self.start;
        self.start += push as f64 * self.span * 0.02;
        self.clamp_view();
        if self.start != before {
            let t = self.t_of(x.clamp(0.0, w));
            self.move_handle(h, t, snap, intents);
        }
        true
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    // ---- editing ----

    pub fn gain(&mut self, v: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let db = if v.abs() < 0.25 {
            0.0
        } else {
            (v * 10.0).round() / 10.0
        };
        self.draft.gain_db = db;
        if self.playing(snap) && !self.paused(snap) {
            intents.push(Intent::LiveGain(self.id, db, self.draft.muted));
        }
    }

    pub fn pan(&mut self, v: f32) {
        self.draft.pan = if v.abs() < 0.02 {
            0.0
        } else {
            (v * 100.0).round() / 100.0
        };
    }

    pub fn speed(&mut self, v: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let v = if (v - 1.0).abs() < 0.02 {
            1.0
        } else {
            (v * 200.0).round() / 200.0
        };
        self.draft.speed = v;
        if self.playing(snap) && !self.paused(snap) {
            intents.push(Intent::LiveSpeedRamp(self.id, v, 40));
        }
    }

    pub fn reset(&mut self, what: &str, snap: &Snapshot, intents: &mut Vec<Intent>) {
        match what {
            "gain" => self.gain(0.0, snap, intents),
            "pan" => self.pan(0.0),
            _ => self.speed(1.0, snap, intents),
        }
        self.push = true;
    }

    pub fn toggle(&mut self, what: &str, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let playing = self.playing(snap);
        match what {
            "loop" => {
                self.draft.looping = !self.draft.looping;
                if playing {
                    intents.push(Intent::LiveLoop(self.id, self.draft.looping));
                }
            }
            "mute" => {
                self.draft.muted = !self.draft.muted;
                if playing && !self.paused(snap) {
                    intents.push(Intent::LiveGain(
                        self.id,
                        self.draft.gain_db,
                        self.draft.muted,
                    ));
                }
            }
            "next" => {
                self.draft.play_next = match self.draft.play_next {
                    Some(_) => None,
                    None => Some("next_cell".to_string()),
                }
            }
            // Stop Page and Stop All: one or the other, or neither (stop nothing).
            "stop-page" | "stop-all" => {
                let s = if what == "stop-page" {
                    StopScope::Page
                } else {
                    StopScope::All
                };
                let on = &mut self.draft.on_play.stop;
                *on = if *on == s { StopScope::None } else { s };
            }
            _ => self.draft.hold_to_play = !self.draft.hold_to_play,
        }
    }

    pub fn set_colour(&mut self, rgb: Option<[u8; 3]>) {
        self.draft.color = rgb
            .filter(|c| *c != model::DEFAULT_CART_COLOR)
            .map(model::hex);
    }

    pub fn current_rgb(&self) -> [u8; 3] {
        model::parse_rgb(self.draft.color.as_deref()).unwrap_or(model::DEFAULT_CART_COLOR)
    }

    pub fn nudge(&mut self, which: &str, d: f64, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let (a, b) = self.trims();
        let h = if which == "in" {
            Handle::In
        } else {
            Handle::Out
        };
        let t = if h == Handle::In { a + d } else { b + d };
        self.move_handle(h, t, snap, intents);
    }

    /// Where Set, I and O put a handle: the playhead while playing, else the cursor.
    fn here(&self, snap: &Snapshot) -> Option<f64> {
        self.playhead(snap).or(self.cursor)
    }

    fn playhead(&self, snap: &Snapshot) -> Option<f64> {
        let rt = snap.runtime.get(&self.id)?;
        if !matches!(rt.state, Cs::Playing | Cs::Stopping) {
            return None;
        }
        let (a, b) = self.trims();
        Some(a + rt.progress as f64 * (b - a))
    }

    fn play_from(&mut self, t: f64, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let (a, b) = self.trims();
        let t = t.clamp(a, (b - MIN_LEN).max(a));
        if self.playing(snap) {
            intents.push(Intent::Seek(self.id, t));
            if self.paused(snap) {
                intents.push(Intent::LiveSpeedRamp(self.id, self.draft.speed, 5));
            }
        } else {
            intents.push(Intent::PreviewAt(self.draft.clone(), t));
        }
    }

    fn play_pause(&mut self, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if self.playing(snap) && !self.paused(snap) {
            intents.push(Intent::LiveSpeedRamp(self.id, 0.0, 5));
        } else if self.playing(snap) {
            intents.push(Intent::LiveSpeedRamp(self.id, self.draft.speed, 5));
        } else if let Some(t) = self.cursor {
            self.play_from(t, snap, intents);
        } else {
            intents.push(Intent::Preview(self.draft.clone()));
        }
    }

    /// Buttons and keys. Returns what the app does next for the ones it handles itself
    /// (Learn, Replace…, OK…) as `false`.
    pub fn action(&mut self, what: &str, snap: &Snapshot, intents: &mut Vec<Intent>) -> bool {
        match what {
            "top-and-tail" => {
                if let Some((a, b)) = self.overview.as_ref().and_then(|o| o.audio_bounds()) {
                    self.set_trims(a, b, snap, intents);
                }
            }
            "zoom-fit" => self.zoom_fit(),
            "stop" => intents.push(Intent::KillCart(self.id)),
            // Back to In: playing carries on from In (paused stays paused there);
            // stopped, the next Play starts at In.
            "to-in" => {
                let (a, _) = self.trims();
                if self.playing(snap) {
                    intents.push(Intent::Seek(self.id, a));
                }
                self.cursor = None;
                if a < self.start || a > self.start + self.span {
                    self.start = a - self.span * 0.1;
                    self.clamp_view();
                }
            }
            "in-clear" => {
                let (_, b) = self.trims();
                self.set_trims(0.0, b, snap, intents);
                self.last = Handle::In;
            }
            "out-clear" => {
                let (a, _) = self.trims();
                self.set_trims(a, self.dur, snap, intents);
                self.last = Handle::Out;
            }
            "loop" => self.toggle("loop", snap, intents),
            "play" | "space" => self.play_pause(snap, intents),
            "in-set" | "i" => {
                if let Some(t) = self.here(snap) {
                    self.move_handle(Handle::In, t, snap, intents);
                }
            }
            "out-set" | "o" => {
                if let Some(t) = self.here(snap) {
                    self.move_handle(Handle::Out, t, snap, intents);
                }
            }
            // The last 3 s before Out: with Loop on, it carries round into In.
            "p" => {
                let (a, b) = self.trims();
                self.play_from((b - 3.0).max(a), snap, intents);
            }
            "zoom-in" => {
                let anchor = self.anchor();
                self.zoom(1.5, anchor);
            }
            "zoom-out" => {
                let anchor = self.anchor();
                self.zoom(1.0 / 1.5, anchor);
            }
            _ => return false,
        }
        true
    }

    pub fn key(&mut self, name: &str, shift: bool, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let step = if shift { 0.1 } else { 0.01 };
        match name {
            "left" | "right" => {
                let d = if name == "left" { -step } else { step };
                let which = if self.last == Handle::In { "in" } else { "out" };
                self.nudge(which, d, snap, intents);
            }
            other => {
                self.action(other, snap, intents);
            }
        }
    }

    // ---- the pop-up buttons ----

    /// The choices for a pop-up button, as native menu rows.
    pub fn choices(&mut self, field: &str, core: &Core, snap: &Snapshot) -> Vec<PopupRow> {
        let c = &self.draft;
        let mut rows: Vec<(Pick, String, bool)> = Vec::new();
        match field {
            "output" => {
                let cur = bubble_project::output_index(&c.output).unwrap_or(0);
                for (i, l) in core.output_labels(&c.output).into_iter().enumerate() {
                    rows.push((Pick::Output(i), l, i == cur));
                }
            }
            "fade-in" => {
                let cur = c.fade_in.map(|f| f.ms).filter(|ms| *ms > 0);
                for ms in model::FADE_PRESETS_MS {
                    let v = (ms > 0).then_some(ms);
                    rows.push((Pick::FadeIn(v), model::fade_text(ms), v == cur));
                }
            }
            "fade-out" => {
                let (labels, values, cur) =
                    model::fade_choices(c, snap.project.settings.default_fade_out_ms);
                for (i, (l, v)) in labels.into_iter().zip(values).enumerate() {
                    rows.push((Pick::FadeOut(v), l, i == cur));
                }
            }
            "x0" | "x1" => {
                let x0 = field == "x0";
                let cur = if x0 { c.slowdown_ms } else { c.speedup_ms };
                let mut choices = vec![500, 1000, 1500, 2000, 3000, 4000, 5000, 8000];
                if !x0 {
                    choices.insert(0, 0);
                }
                for ms in choices {
                    let p = if x0 {
                        Pick::SlowDown(ms)
                    } else {
                        Pick::SpeedUp(ms)
                    };
                    rows.push((p, seconds_text(ms), ms == cur));
                }
            }
            "retrigger" => {
                let default = snap.project.settings.default_retrigger;
                let cur = c.retrigger.unwrap_or(default);
                for r in [
                    Retrigger::Restart,
                    Retrigger::ToggleStop,
                    Retrigger::Overlap,
                    Retrigger::Ignore,
                ] {
                    let v = (r != default).then_some(r);
                    rows.push((Pick::Retrigger(v), retrigger_text(r).to_string(), r == cur));
                }
            }
            _ => {
                rows.push((Pick::Group(None), "None".into(), c.group.is_none()));
                for (i, g) in snap.project.groups.iter().enumerate() {
                    rows.push((
                        Pick::Group(Some(g.id)),
                        g.display_name(i),
                        c.group == Some(g.id),
                    ));
                }
            }
        }
        self.popup = rows.iter().map(|r| r.0.clone()).collect();
        rows.into_iter()
            .enumerate()
            .map(|(i, (_, text, on))| PopupRow {
                id: i as i32,
                text: text.into(),
                enabled: true,
                checked: on,
            })
            .collect()
    }

    pub fn chosen(&mut self, id: i32) {
        let Some(p) = usize::try_from(id)
            .ok()
            .and_then(|i| self.popup.get(i).cloned())
        else {
            return;
        };
        let c = &mut self.draft;
        match p {
            Pick::Output(i) => c.output = bubble_project::output_letter(i),
            Pick::FadeIn(v) => {
                let curve = c.fade_in.map(|f| f.curve).unwrap_or_default();
                c.fade_in = v.map(|ms| FadeSpec {
                    curve,
                    ..FadeSpec::ms(ms)
                });
            }
            Pick::FadeOut(v) => {
                let curve = c.fade_out.map(|f| f.curve).unwrap_or_default();
                c.fade_out = v.map(|ms| FadeSpec {
                    curve,
                    ..FadeSpec::ms(ms)
                });
            }
            Pick::SlowDown(ms) => c.slowdown_ms = ms,
            Pick::SpeedUp(ms) => c.speedup_ms = ms,
            Pick::Retrigger(r) => c.retrigger = r,
            Pick::Group(g) => c.group = g,
        }
    }

    // ---- showing it ----

    /// The cart in the session changed underneath (its audio replaced or relinked):
    /// the draft follows its file and trims.
    fn follow(&mut self, cart: &Cart) {
        if cart.asset != self.base.asset {
            self.draft.asset = cart.asset;
            self.draft.trim = cart.trim;
            self.base.asset = cart.asset;
            self.base.trim = cart.trim;
            self.overview = None;
            self.dur = 0.0;
            self.cursor = None;
        }
        // Bindings live in the session, not the cart: nothing else to follow.
    }

    pub fn show(&mut self, core: &mut Core, snap: &Snapshot) {
        let Some(cart) = snap.project.cart(self.id).cloned() else {
            return;
        };
        self.follow(&cart);
        let win = self.win.clone_strong();

        // The file: its length, and its overview once read.
        let mut message = String::new();
        let mut details = String::new();
        if let Some(a) = self.draft.asset {
            let mut ctl = core.lock();
            match ctl.asset_state(a) {
                Some(AssetState::Ready { source, .. }) => {
                    let (frames, rate, ch) = (source.frames, source.sample_rate, source.channels);
                    if self.dur <= 0.0 {
                        self.frames = frames;
                        self.rate = rate.max(1) as f64;
                        self.channels = ch;
                        self.dur = frames as f64 / self.rate;
                        self.zoom_fit();
                    }
                    if self.overview.is_none() {
                        self.overview = ctl.asset_overview(a);
                    }
                }
                _ => {
                    message = snap
                        .asset_info
                        .get(&a)
                        .cloned()
                        .unwrap_or_else(|| "Loading the audio file…".into());
                }
            }
            let file = snap
                .project
                .asset(a)
                .map(|x| x.path.rsplit(['/', '\\']).next().unwrap_or("").to_string())
                .unwrap_or_default();
            if self.dur > 0.0 {
                let ch = match self.channels {
                    1 => "mono".to_string(),
                    2 => "stereo".to_string(),
                    n => format!("{n} ch"),
                };
                details = format!(
                    "{file} · {} kHz · {ch} · {}",
                    self.rate / 1000.0,
                    fmt_time(self.dur)
                );
            } else {
                details = file;
            }
        } else {
            message = "No audio file assigned.".into();
        }
        let has_audio = self.dur > 0.0;
        // While playing, the view turns the page to keep the playhead in sight.
        if let Some(t) = self.playhead(snap)
            && self.drag.is_none()
            && (t < self.start || t > self.start + self.span)
        {
            self.start = t - self.span * 0.05;
            self.clamp_view();
        }

        // The waveform for the part on screen.
        let (w, h) = (win.get_wave_width(), win.get_wave_height());
        let ov = self.overview.clone();
        let sig = (
            self.start.to_bits(),
            self.span.to_bits(),
            w as u32,
            h as u32,
            ov.as_ref().map_or(0, |o| Arc::as_ptr(o) as usize),
        );
        if has_audio && sig != self.wave_sig {
            self.wave_sig = sig;
            win.set_wave_path(self.wave_path(ov.as_deref(), w, h).into());
        }
        let (a, b) = self.trims();
        let (in_x, out_x) = (self.x_of(a), self.x_of(b));
        let mut fade = String::new();
        if has_audio {
            if let Some(f) = self.draft.fade_in.filter(|f| f.ms > 0) {
                let e = self.x_of((a + f.ms as f64 / 1000.0).min(b));
                fade += &format!("M{in_x:.1} {h:.1}L{e:.1} 0");
            }
            let fo = self
                .draft
                .fade_out
                .map_or(snap.project.settings.default_fade_out_ms, |f| f.ms);
            if self.draft.fade_out_at_end && fo > 0 {
                let s = self.x_of((b - fo as f64 / 1000.0).max(a));
                fade += &format!("M{s:.1} 0L{out_x:.1} {h:.1}");
            }
        }
        win.set_fade_path(fade.into());
        win.set_in_x(if has_audio { in_x } else { -100.0 });
        win.set_out_x(if has_audio { out_x } else { -100.0 });
        win.set_in_tab(has_audio && in_x < 0.0);
        win.set_out_tab(has_audio && out_x > w);
        win.set_in_time(fmt_time(a).into());
        win.set_out_time(fmt_time(b).into());
        win.set_in_chars(model::chars(&fmt_time(a)));
        win.set_out_chars(model::chars(&fmt_time(b)));
        // While it plays, the playhead stands for the cursor.
        let playhead = self.playhead(snap);
        win.set_cursor_x(match (self.cursor, playhead) {
            (Some(t), None) => self.x_of(t),
            _ => -100.0,
        });
        win.set_playhead_x(playhead.map_or(-100.0, |t| self.x_of(t)));
        win.set_ruler(ModelRc::new(VecModel::from(if has_audio {
            self.ruler(w)
        } else {
            Vec::new()
        })));

        // Transport and meter.
        let rt = snap.runtime.get(&self.id).copied();
        let playing = playhead.is_some();
        let (pos, rem) = match rt.filter(|_| playing) {
            Some(r) => (r.elapsed_s as f64, r.remaining_s as f64),
            None => (0.0, (b - a).max(0.0) / self.draft.speed.max(0.01) as f64),
        };
        win.set_elapsed(model::chars(&fmt_time(pos)));
        win.set_remaining(model::chars(&format!("−{}", fmt_time(rem))));
        win.set_running(playing && !self.paused(snap));
        let dt = self.last_meter.elapsed().as_secs_f32();
        self.last_meter = Instant::now();
        self.ppm.update(
            core.take_peaks().get(&self.id).copied().unwrap_or_default(),
            dt,
        );
        let (lv, hd) = (self.ppm.level(), self.ppm.hold());
        let colour = |db: f32| match zone(db) {
            2 => Color::from_rgb_u8(0xE5, 0x39, 0x35),
            1 => Color::from_rgb_u8(0xFB, 0x8C, 0x00),
            _ => Color::from_rgb_u8(0x43, 0xA0, 0x47),
        };
        win.set_level_l(scale_pos(lv[0]));
        win.set_level_r(scale_pos(lv[1]));
        win.set_hold_l(scale_pos(hd[0]));
        win.set_hold_r(scale_pos(hd[1]));
        win.set_hold_l_colour(colour(hd[0]));
        win.set_hold_r_colour(colour(hd[1]));

        // The settings.
        let c = &self.draft;
        let default_out = snap.project.settings.default_fade_out_ms;
        let outputs = core.output_labels(&c.output);
        let out_i = bubble_project::output_index(&c.output).unwrap_or(0);
        let (fades, _, fo_i) = model::fade_choices(c, default_out);
        let group = c
            .group
            .and_then(|g| snap.project.groups.iter().position(|x| x.id == g))
            .map_or("None".to_string(), |i| {
                snap.project.groups[i].display_name(i)
            });
        let keys: Vec<String> = snap
            .project
            .bindings
            .iter()
            .filter(|b| b.targets.contains(&self.id))
            .filter(|b| matches!(b.source, BindingSource::Key { .. }))
            .map(|b| describe(&b.source))
            .collect();
        let midi: Vec<String> = snap
            .project
            .bindings
            .iter()
            .filter(|b| b.targets.contains(&self.id))
            .filter(|b| matches!(b.source, BindingSource::Midi { .. }))
            .map(|b| describe(&b.source))
            .collect();
        let rgb = self.current_rgb();
        let swatch = model::SWATCHES
            .iter()
            .position(|s| *s == rgb)
            .map_or(-1, |i| i as i32);
        let name = model::piped(&bubble_control::display_label(c, &snap.project));
        let info = SetupInfo {
            name: name.clone().into(),
            details: details.into(),
            has_audio,
            message: message.into(),
            output: outputs.get(out_i).cloned().unwrap_or_default().into(),
            fade_in: model::fade_text(c.fade_in.map_or(0, |f| f.ms)).into(),
            fade_out: fades.get(fo_i).cloned().unwrap_or_default().into(),
            speed_x0: seconds_text(c.slowdown_ms).into(),
            speed_x1: seconds_text(c.speedup_ms).into(),
            retrigger: retrigger_text(
                c.retrigger
                    .unwrap_or(snap.project.settings.default_retrigger),
            )
            .into(),
            play_next: c.play_next.is_some(),
            stop_page: c.on_play.stop == StopScope::Page,
            stop_all: c.on_play.stop == StopScope::All,
            group: group.into(),
            gain_text: gain_text(c.gain_db).into(),
            pan_text: pan_text(c.pan).into(),
            speed_text: format!("{:.0}%", c.speed * 100.0).into(),
            looping: c.looping,
            muted: c.muted,
            hold: c.hold_to_play,
            swatches: ModelRc::new(VecModel::from(
                model::SWATCHES
                    .iter()
                    .map(|s| Color::from_rgb_u8(s[0], s[1], s[2]))
                    .collect::<Vec<_>>(),
            )),
            colour: swatch,
            hotkey: keys.join("  ").into(),
            cart_color: Color::from_rgb_u8(rgb[0], rgb[1], rgb[2]),
            midi: midi.join(", ").into(),
            show_midi: !core.prefs.midi_disabled || !midi.is_empty(),
            learning: snap.learning.is_some_and(|l| l.cart == self.id),
            missing: c.asset.is_some_and(|a| snap.missing.contains(&a)),
        };
        if win.get_info() != info {
            win.set_info(info);
        }
        let title: SharedString = format!("Cart Setup - {name}").into();
        if win.get_window_title() != title {
            win.set_window_title(title);
        }
        if std::mem::take(&mut self.push) {
            win.set_gain(c.gain_db);
            win.set_pan(c.pan);
            win.set_speed(c.speed);
            win.set_label_text(model::piped(&c.label).into());
            win.set_notes_text(c.notes.clone().into());
        }
    }

    /// The envelope of the visible part: one vertical stroke per pixel column.
    fn wave_path(&self, ov: Option<&Overview>, w: f32, h: f32) -> String {
        let Some(ov) = ov.filter(|o| o.max.len() > 1) else {
            return format!("M0 {:.1}L{:.1} {:.1}", h / 2.0, w, h / 2.0);
        };
        let n = ov.max.len();
        let cols = w.max(1.0) as usize;
        let mid = h / 2.0;
        let amp = h / 2.0 * 0.94;
        let mut s = String::with_capacity(cols * 20);
        for col in 0..cols {
            let t0 = self.start + col as f64 / cols as f64 * self.span;
            let t1 = self.start + (col + 1) as f64 / cols as f64 * self.span;
            if t1 <= 0.0 || t0 >= ov.duration_s {
                continue;
            }
            let i0 = ((t0 / ov.duration_s) * n as f64).floor().max(0.0) as usize;
            let i1 = (((t1 / ov.duration_s) * n as f64).ceil() as usize).clamp(i0 + 1, n);
            if i0 >= n {
                break;
            }
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for i in i0..i1 {
                lo = lo.min(ov.min[i]);
                hi = hi.max(ov.max[i]);
            }
            let y1 = mid - hi.clamp(-1.0, 1.0) * amp;
            let y2 = (mid - lo.clamp(-1.0, 1.0) * amp).max(y1 + 0.6);
            s += &format!("M{}.5 {y1:.1}V{y2:.1}", col);
        }
        s
    }

    /// Time marks about every 110 px, at a round step.
    fn ruler(&self, w: f32) -> Vec<WaveMark> {
        let want = self.span * 110.0 / w.max(1.0) as f64;
        let steps = [
            0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
            600.0,
        ];
        let step = steps.iter().copied().find(|s| *s >= want).unwrap_or(600.0);
        let mut out = Vec::new();
        let mut t = (self.start.max(0.0) / step).ceil() * step;
        while t <= (self.start + self.span).min(self.dur) && out.len() < 40 {
            let m = (t / 60.0).floor();
            let sec = t - m * 60.0;
            let text = if step < 0.1 {
                format!("{}:{:05.2}", m as u32, sec)
            } else if step < 1.0 {
                format!("{}:{:04.1}", m as u32, sec)
            } else {
                format!("{}:{:02.0}", m as u32, sec.floor())
            };
            out.push(WaveMark {
                x: self.x_of(t),
                text: text.into(),
            });
            t += step;
        }
        out
    }
}
