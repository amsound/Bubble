//! What every interface shows the same way: the text on a cart tile, the window title,
//! the status line, the busy strip, and where the keyboard cursor and new carts go.
//! Pure text and positions; drawing them is the interface's job.

use std::time::Instant;

use bubble_control::{CartRuntime, CartState, describe};
use bubble_project::{Cart, Project, StopScope};

use crate::app::{Core, Snapshot};

/// Tile clock: MM:SS (H:MM:SS from an hour). `round_up` for durations and
/// countdowns, so a 0.9 s cart reads 00:01 and a countdown reaches 00:00 at the end.
pub fn fmt_clock(s: f32, round_up: bool) -> String {
    if !s.is_finite() {
        return "∞".into();
    }
    let s = s.max(0.0);
    let t = if round_up {
        (s - 1e-3).max(0.0).ceil()
    } else {
        s.floor()
    } as u64;
    let (h, m, sec) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// Precise time for editing windows: MM:SS.s
pub fn fmt_precise(s: f32) -> String {
    if !s.is_finite() {
        return "∞".into();
    }
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{:02}:{:04.1}", m as u32, s - m * 60.0)
}

/// Whether the In and Out points differ from the file's start and end.
pub fn trim_flags(cart: &Cart, project: &Project) -> (bool, bool) {
    let Some(t) = cart.trim else {
        return (false, false);
    };
    let total = cart
        .asset
        .and_then(|a| project.asset(a))
        .and_then(|a| a.meta.as_ref())
        .map(|m| m.frames);
    let out_trimmed = match total {
        Some(n) => t.out_frames < n,
        // Unknown length: a trim that does not move In must have moved Out.
        None => t.in_frames == 0,
    };
    (t.in_frames > 0, out_trimmed)
}

/// Short trigger names for the tile, e.g. "F1", "Meta+1", "N60 ch1".
pub fn trigger_names(cart: &Cart, project: &Project) -> Vec<String> {
    project
        .bindings
        .iter()
        .filter(|b| b.targets.contains(&cart.id))
        .map(|b| describe(&b.source))
        .collect()
}

/// "G1" or "G1 Drums" for the cart's exclusive group.
pub fn group_label(cart: &Cart, project: &Project) -> Option<String> {
    let g = cart.group?;
    project
        .groups
        .iter()
        .position(|x| x.id == g)
        .map(|i| project.groups[i].label(i))
}

/// The small text on a tile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TileText {
    /// "Out A"; empty for a control button.
    pub output: String,
    /// After the output: trim (`>`, `<`, `X`), loop, speed, MUTE, HOLD, stop-others,
    /// play-next and file-changed marks. A control button has its name here instead.
    pub flags: Vec<String>,
    /// Remaining time while playing, the duration when ready, or the state
    /// (loading…, MISSING, ERROR). '@' marks a time affected by a speed change.
    pub time: String,
}

pub fn tile_text(cart: &Cart, project: &Project, rt: &CartRuntime) -> TileText {
    if let Some(k) = cart.control {
        return TileText {
            output: String::new(),
            flags: vec![k.name().to_uppercase()],
            time: String::new(),
        };
    }
    let playing = matches!(rt.state, CartState::Playing | CartState::Stopping);
    let mut flags: Vec<String> = Vec::new();
    match trim_flags(cart, project) {
        // In and Out both trimmed.
        (true, true) => flags.push("X".into()),
        (true, false) => flags.push(">".into()),
        (false, true) => flags.push("<".into()),
        _ => {}
    }
    if cart.looping {
        flags.push("⟲".into());
    }
    let shown_speed = if playing { rt.speed } else { cart.speed };
    // Held at speed 0 (paused in Cart Setup) is paused, not a speed change.
    let paused = playing && rt.speed < 0.005;
    let sped = !paused && (shown_speed - 1.0).abs() > 0.005;
    if sped {
        flags.push(format!("×{shown_speed:.2}"));
    }
    if cart.muted {
        flags.push("MUTE".into());
    }
    if cart.hold_to_play {
        flags.push("HOLD".into());
    }
    if cart.on_play.stop != StopScope::None {
        flags.push("■".into());
    }
    if cart.play_next.is_some() {
        flags.push("→".into());
    }
    if rt.changed {
        flags.push("⚠".into());
    }
    let time = match rt.state {
        CartState::Playing | CartState::Stopping => {
            format!("-{}", fmt_clock(rt.remaining_s, true))
        }
        CartState::Ready => fmt_clock(rt.duration_s, true),
        CartState::Loading => "loading…".into(),
        CartState::Missing => "MISSING".into(),
        CartState::Error => "ERROR".into(),
        CartState::Empty => String::new(),
    };
    // As in the reference: '@' marks a time affected by a speed change.
    let time = if sped && !time.is_empty() && rt.state != CartState::Missing {
        format!("@{time}")
    } else {
        time
    };
    TileText {
        output: format!("Out {}", cart.output),
        flags,
        time,
    }
}

/// How much a status message matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warn,
    Error,
}

/// One part of the status line.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusItem {
    pub text: String,
    pub level: Level,
}

/// What Bubble is busy with (opening, importing, copying, converting, preparing audio),
/// for a strip that keeps long operations from looking like a hang.
#[derive(Clone, Debug, PartialEq)]
pub struct Busy {
    pub text: String,
    /// Audio preparation: fraction of files ready.
    pub progress: Option<f32>,
}

/// The first free cell of `page`, row by row.
pub fn first_empty_cell(project: &Project, page: usize) -> Option<(usize, u16, u16)> {
    let p = project.pages.get(page)?;
    (0..p.rows)
        .flat_map(|r| (0..p.cols).map(move |c| (r, c)))
        .find(|(r, c)| p.cell(*r, *c).is_none())
        .map(|(r, c)| (page, r, c))
}

/// Where Duplicate Cart puts a copy: the next free cell after the cart on its page,
/// wrapping round to the start. `None` when the page is full.
pub fn free_cell_after(project: &Project, id: uuid::Uuid) -> Option<(usize, u16, u16)> {
    let (p, r, c) = project.cart_position(id)?;
    let page = &project.pages[p];
    let cols = page.cols as u32;
    let start = r as u32 * cols + c as u32 + 1;
    let total = page.rows as u32 * cols;
    (start..total)
        .chain(0..start)
        .map(|i| ((i / cols) as u16, (i % cols) as u16))
        .find(|(r, c)| page.cell(*r, *c).is_none())
        .map(|(r, c)| (p, r, c))
}

/// One step of the keyboard cursor from (row, col), wrapping across rows.
pub fn step_cell(rows: u16, cols: u16, at: (u16, u16), dr: i32, dc: i32) -> (u16, u16) {
    let (rows, cols) = (rows.max(1) as i32, cols.max(1) as i32);
    let idx = (at.0 as i32 * cols + at.1 as i32 + dr * cols + dc).rem_euclid(rows * cols);
    ((idx / cols) as u16, (idx % cols) as u16)
}

impl Core {
    /// Window title: the session and whether it is saved, as in the reference.
    pub fn window_title(&self, snap: &Snapshot) -> String {
        match &snap.path {
            None => "Bubble - No session".to_string(),
            Some(_) => {
                let saved = self.saved_at.map(crate::app::saved_text);
                match (snap.dirty, saved) {
                    (false, Some(t)) => format!("Bubble - {} > {t}", snap.project.name),
                    (true, Some(t)) => format!(
                        "Bubble - {} > Unsaved changes ({})",
                        snap.project.name,
                        t.replacen("Last saved", "last saved", 1)
                    ),
                    (true, None) => format!("Bubble - {} > Unsaved changes", snap.project.name),
                    (false, None) => format!("Bubble - {}", snap.project.name),
                }
            }
        }
    }

    /// The status line: audio problems, missing files, cloud folders, failing saves
    /// and MIDI activity. Empty when there is nothing to say.
    pub fn status_items(&self, snap: &Snapshot) -> Vec<StatusItem> {
        let item = |text: String, level| StatusItem { text, level };
        let mut out = Vec::new();
        if let Some(e) = &self.audio_error {
            out.push(item(format!("No audio: {e}"), Level::Error));
        } else if let Some(n) = self.audio_notice() {
            out.push(item(n, Level::Warn));
        } else if !self.output_errors.is_empty() {
            out.push(item(
                "Some outputs are unavailable (Audio ▸ Outputs)".into(),
                Level::Warn,
            ));
        }
        if self.audio_error.is_none()
            && let Some((_, name, rate)) = self.rate_mismatches().first()
        {
            out.push(item(
                format!(
                    "{name} is at {}, Bubble at {}: change it in Windows Sound settings",
                    khz(*rate),
                    khz(self.prefs.rate())
                ),
                Level::Warn,
            ));
        }
        if !snap.missing.is_empty() {
            out.push(item(
                format!("{} missing file(s)", snap.missing.len()),
                Level::Warn,
            ));
        }
        // A session in a cloud-synced folder is only logged; the status line says
        // nothing. An alert still comes if any of its audio is online-only.
        // Never let a failing autosave or preferences save go unnoticed.
        for p in self
            .autosave_problem
            .iter()
            .chain(self.prefs_problem.iter())
        {
            out.push(item(p.clone(), Level::Error));
        }
        if self.midi.active_recently(150) {
            out.push(item("● MIDI".into(), Level::Error));
        }
        out
    }

    /// What Bubble is busy with, if anything. Call once per frame: it also keeps the
    /// "n of total" count of audio files being prepared.
    pub fn busy(&mut self, snap: &Snapshot) -> Option<Busy> {
        // Audio preparation: the largest pending count seen is the total.
        if snap.pending == 0 {
            self.loading_total = 0;
        } else {
            self.loading_total = self.loading_total.max(snap.pending);
        }
        let opening = self.opening.as_ref().map(|(p, _, _)| {
            format!(
                "Opening {}…",
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            )
        });
        let task = self.job.as_ref().map(|j| j.label.clone()).or(opening);
        let loading = (snap.pending > 0).then(|| {
            let total = self.loading_total.max(1);
            let done = total - snap.pending.min(total);
            (done, total)
        });
        match (task, loading) {
            (None, None) => None,
            (Some(text), l) => Some(Busy {
                text,
                progress: l.map(|(d, t)| d as f32 / t as f32),
            }),
            (None, Some((done, total))) => Some(Busy {
                text: format!("Preparing audio: {done} of {total} files ready"),
                progress: Some(done as f32 / total as f32),
            }),
        }
    }

    /// Logs once when the open session's audio has finished loading.
    pub fn note_loaded(&mut self, snap: &Snapshot) {
        if snap.pending == 0
            && let Some(t) = self.loading_since.take()
        {
            let (used, _) = self.lock().audio_memory();
            crate::log::info(&format!(
                "audio ready in {:.1} s ({:.0} MB in memory, {} missing)",
                t.elapsed().as_secs_f32(),
                used as f64 / 1e6,
                snap.missing.len()
            ));
        }
    }

    /// Copies audio from outside the session into it once nothing else is running.
    pub fn collect_when_idle(&mut self) {
        if self.want_collect && self.job.is_none() && self.opening.is_none() {
            self.want_collect = false;
            self.collect_external();
        }
    }

    /// For testing crash handling: `BUBBLE_CRASH_TEST=1` panics a few seconds in.
    pub fn crash_test(&self) {
        if std::env::var_os("BUBBLE_CRASH_TEST").is_some()
            && self.started.elapsed() > std::time::Duration::from_secs(3)
        {
            panic!("crash test requested by BUBBLE_CRASH_TEST");
        }
    }
}

/// Keeps the window's place for preferences: changes are saved a second after they
/// stop (dragging a window reports many), and on exit.
#[derive(Default)]
pub struct PlaceTracker {
    changed: Option<Instant>,
}

impl PlaceTracker {
    /// Records the window's current place. Returns true when it is time to save.
    pub fn update(&mut self, core: &mut Core, place: crate::prefs::WindowPlace) -> bool {
        // When maximised, keep the un-maximised place to return to.
        let place = match (place.maximized, core.prefs.window) {
            (true, Some(w)) => crate::prefs::WindowPlace {
                maximized: true,
                ..w
            },
            _ => place,
        };
        if core.prefs.window != Some(place) {
            core.prefs.window = Some(place);
            self.changed = Some(Instant::now());
        }
        self.due()
    }

    /// Whether a change has settled and should be saved now (then clears it).
    pub fn due(&mut self) -> bool {
        if self
            .changed
            .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(1))
        {
            self.changed = None;
            true
        } else {
            false
        }
    }

    pub fn pending(&self) -> bool {
        self.changed.is_some()
    }
}

/// "48 kHz", "44.1 kHz".
pub fn khz(rate: u32) -> String {
    format!("{} kHz", rate as f32 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format() {
        assert_eq!(fmt_clock(0.916, true), "00:01");
        assert_eq!(fmt_clock(0.0, true), "00:00");
        assert_eq!(fmt_clock(20.0, true), "00:20");
        assert_eq!(fmt_clock(83.4, false), "01:23");
        assert_eq!(fmt_clock(3725.0, false), "1:02:05");
        assert_eq!(fmt_precise(83.46), "01:23.5");
    }

    #[test]
    fn cursor_wraps() {
        assert_eq!(step_cell(2, 3, (0, 2), 0, 1), (1, 0));
        assert_eq!(step_cell(2, 3, (0, 0), 0, -1), (1, 2));
        assert_eq!(step_cell(2, 3, (1, 1), 1, 0), (0, 1));
        assert_eq!(step_cell(2, 3, (0, 1), -1, 0), (1, 1));
    }

    #[test]
    fn tile_text_marks() {
        let project = bubble_control::Controller::new_project("T");
        let mut cart = Cart::new();
        cart.looping = true;
        cart.muted = true;
        let rt = CartRuntime {
            state: CartState::Ready,
            duration_s: 61.2,
            speed: 1.0,
            ..Default::default()
        };
        let t = tile_text(&cart, &project, &rt);
        assert_eq!(t.output, "Out A");
        assert_eq!(t.flags, vec!["⟲", "MUTE"]);
        assert_eq!(t.time, "01:02");
        cart.speed = 0.5;
        assert_eq!(tile_text(&cart, &project, &rt).time, "@01:02");
        cart.control = Some(bubble_project::Control::StopAll);
        let t = tile_text(&cart, &project, &rt);
        assert_eq!((t.output.as_str(), t.flags[0].as_str()), ("", "STOP ALL"));
    }
}
