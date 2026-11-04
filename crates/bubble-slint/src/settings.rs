//! The Settings window (this computer: `Prefs`) and Session Settings (this session:
//! `ProjectSettings`, its groups and pages), each in its own window (ui/settings.slint).
//!
//! Both change things as they are edited, except the outputs, sample rate and buffer
//! size: applying those restarts audio and stops whatever plays, so they are a draft
//! until Apply. Text fields are only refilled from Rust when what they show changed
//! from outside (an undo, the menus), so typing in them is never interrupted.

use std::time::{Duration, Instant};

use bubble_core::Intent;
use bubble_core::Snapshot;
use bubble_core::app::Core;
use bubble_core::menu::{self, Cmd};
use bubble_core::outputs;
use bubble_core::prefs::{Appearance, OutputPref};
use bubble_project::{DuplicatePolicy, MouseMode, Retrigger, Scope};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{
    GroupRow, OutputRow, OutputUse, PageRow, PopupRow, PortRow, SessionSettingsWindow,
    SettingsWindow, StatusLine,
};

/// A label in this platform's style: Title Case on a Mac, sentence case on Windows.
fn name(mac: &str, other: &str) -> String {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
    .to_string()
}

fn rows(list: Vec<(String, bool, bool)>) -> ModelRc<PopupRow> {
    let rows: Vec<PopupRow> = list
        .into_iter()
        .enumerate()
        .map(|(i, (text, enabled, checked))| PopupRow {
            id: i as i32,
            text: text.into(),
            enabled,
            checked,
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// Replaces a model only when what it shows changed (a new model rebuilds its rows,
/// text fields included).
fn refresh<T: Clone + PartialEq + 'static>(
    shown: &mut Vec<T>,
    new: Vec<T>,
    set: impl FnOnce(ModelRc<T>),
) {
    if *shown != new {
        *shown = new.clone();
        set(ModelRc::new(VecModel::from(new)));
    }
}

/// Audio status: the summary, whether there are dropouts, and the details (the open
/// devices first, each "Output A, B").
fn status(core: &Core, details: bool) -> (String, bool, Vec<StatusLine>) {
    let line = |label: String, value: String, bad: bool| StatusLine {
        label: label.into(),
        value: value.into(),
        bad,
    };
    let c = core.lock();
    let diags: Vec<_> = c
        .engines()
        .iter()
        .map(|e| e.status().diagnostics())
        .collect();
    let (used, limit) = c.audio_memory();
    drop(c);
    let xruns: u64 = diags.iter().map(|d| d.xruns).sum();
    let late: u64 = diags.iter().map(|d| d.stream_underruns).sum();
    let lost: u64 = diags.iter().map(|d| d.stream_underrun_frames).sum();
    let count = |n: u64| {
        if n == 1 {
            "1 dropout".to_string()
        } else {
            format!("{n} dropouts")
        }
    };
    let total = xruns + late;
    let summary = match (xruns, late) {
        (0, 0) => "All well · 0 dropouts this run".to_string(),
        (x, 0) => format!("{} this run: {x} device short", count(total)),
        (0, l) => format!(
            "{} this run: {l} late disk read{}",
            count(total),
            if l == 1 { "" } else { "s" }
        ),
        (x, l) => format!(
            "{} this run: {x} device short, {l} late disk read{}",
            count(total),
            if l == 1 { "" } else { "s" }
        ),
    };
    if !details {
        return (summary, total > 0, Vec::new());
    }
    let mut lines = Vec::new();
    let outputs = core.prefs.effective_outputs();
    for (i, (a, key)) in core.audio.iter().zip(&core.audio_keys).enumerate() {
        let letters: Vec<String> = outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| &o.device == key)
            .map(|(n, _)| bubble_project::output_letter(n))
            .collect();
        let label = if letters.len() > 1 {
            format!("Outputs {}", letters.join(", "))
        } else {
            format!(
                "Output {}",
                letters.first().cloned().unwrap_or_else(|| "A".into())
            )
        };
        let latency = diags
            .get(i)
            .map(|d| d.output_latency_us)
            .filter(|us| *us > 0)
            .map_or(String::new(), |us| {
                format!(" · {:.1} ms", us as f32 / 1000.0)
            });
        lines.push(line(
            label,
            format!(
                "{} · {} kHz · {} ch · {}{latency}",
                a.device_name,
                a.sample_rate as f32 / 1000.0,
                a.channels,
                a.sample_format
            ),
            false,
        ));
    }
    let rate = diags.first().map_or(48_000, |d| d.sample_rate.max(1));
    let ms = |frames: u64| frames as f32 * 1000.0 / rate as f32;
    let asked = match core.prefs.buffer_frames {
        Some(f) => format!("{f} frames ({:.1} ms)", ms(f as u64)),
        None => name("Device Default", "Device default"),
    };
    lines.push(line(
        name("Buffer Asked For", "Buffer asked for"),
        asked,
        false,
    ));
    let (lo, hi) = diags.iter().fold((u32::MAX, 0), |a, d| {
        (
            a.0.min(d.min_callback_frames.max(1)),
            a.1.max(d.max_callback_frames),
        )
    });
    lines.push(line(
        name("Buffer Used", "Buffer used"),
        if hi == 0 {
            "–".into()
        } else if lo == hi {
            format!("{hi} frames")
        } else {
            format!("{lo} to {hi} frames")
        },
        false,
    ));
    let busiest = diags.iter().map(|d| d.max_process_ns).max().unwrap_or(0);
    let budget = diags
        .iter()
        .map(|d| d.last_callback_frames)
        .max()
        .unwrap_or(0);
    lines.push(line(
        name("Busiest Callback", "Busiest callback"),
        format!("{} µs of {:.1} ms", busiest / 1000, ms(budget as u64)),
        false,
    ));
    let (playing, capacity, stolen) = core.lock().engines().iter().fold((0, 0, 0), |a, e| {
        let d = e.status().diagnostics();
        (
            a.0 + d.active_voices,
            a.1 + e.status().voice_capacity(),
            a.2 + d.voices_stolen,
        )
    });
    // Plays at once (an overlapping cart can have several), out of the most there
    // can be, and how many were cut short to make room.
    lines.push(line(
        "Playing".into(),
        format!("{playing} of {capacity} at once · {stolen} cut short"),
        false,
    ));
    lines.push(line(
        name("Device Ran Short", "Device ran short"),
        if xruns == 0 {
            "None".into()
        } else {
            count(xruns)
        },
        xruns > 0,
    ));
    lines.push(line(
        name("File Read Late", "File read late"),
        if late == 0 {
            "None".into()
        } else {
            format!("{} · {:.0} ms lost", count(late), ms(lost))
        },
        late > 0,
    ));
    let mb = |b: u64| (b as f64 / 1_048_576.0).round() as u64;
    lines.push(line(
        name("Audio Memory", "Audio memory"),
        format!("{} MB of {} MB", mb(used), mb(limit)),
        false,
    ));
    let wait = core
        .midi
        .max_lock_wait_us
        .load(std::sync::atomic::Ordering::Relaxed);
    let ports = core.midi.open_ports().len();
    lines.push(line(
        "MIDI".into(),
        format!(
            "{ports} {} · {:.1} ms wait",
            if ports == 1 { "port" } else { "ports" },
            wait as f32 / 1000.0
        ),
        false,
    ));
    (summary, total > 0, lines)
}

pub fn appearance_text(a: Appearance) -> String {
    menu::APPEARANCES
        .iter()
        .find(|(_, x)| *x == a)
        .map_or("Follow System", |(n, _)| n)
        .to_string()
}

fn mouse_text(m: MouseMode) -> String {
    match m {
        MouseMode::Toggle => name("Plays or Stops It", "Plays or stops it"),
        MouseMode::LeftPlayRightStop => name(
            "Plays It (Right-Click Stops)",
            "Plays it (right-click stops)",
        ),
    }
}

fn bar_position_text(top: bool) -> String {
    if top { "Top" } else { "Bottom" }.into()
}

fn rate_text(r: u32) -> String {
    let khz = format!("{} kHz", r as f32 / 1000.0);
    if r == bubble_core::prefs::DEFAULT_SAMPLE_RATE {
        format!("{khz} (default)")
    } else {
        khz
    }
}

fn level_text(db: f32) -> String {
    if db.abs() < 0.05 {
        "0 dB".into()
    } else {
        format!("{:.1} dB", db).replace('-', "−")
    }
}

// ---------------------------------------------------------------------------------
// Settings (this computer)
// ---------------------------------------------------------------------------------

/// What a Settings pop-up choice does.
#[derive(Clone)]
enum Pick {
    Appearance(Appearance),
    Bar(u8),
    BarTop(bool),
    TileProgress(u8),
    Mouse(MouseMode),
    Rate(u32),
    Buffer(Option<u32>),
    /// Output (row) to a device ("" = the system default), on its first free channels.
    Device(usize, String),
    Channels(usize, u16, u16),
}

pub struct Settings {
    pub win: SettingsWindow,
    /// The tallest section seen, so the window grows only when a taller one is shown
    /// (and stays as the user sizes it otherwise).
    pub tallest: f32,
    /// The outputs, sample rate and buffer size as edited (applied by Apply).
    outputs: Vec<OutputPref>,
    rate: u32,
    buffer: Option<u32>,
    picks: Vec<Pick>,
    shown_outputs: Vec<OutputRow>,
    shown_ports: Vec<PortRow>,
    shown_level: Option<f32>,
    shown_status: (Vec<StatusLine>, Vec<StatusLine>, Vec<StatusLine>),
    /// The output level moves with the slider; it is saved once it rests.
    save_at: Option<Instant>,
}

impl Settings {
    pub fn new(win: SettingsWindow, core: &Core) -> Self {
        let mut s = Settings {
            tallest: 0.0,
            win,
            outputs: Vec::new(),
            rate: 0,
            buffer: None,
            picks: Vec::new(),
            shown_outputs: Vec::new(),
            shown_ports: Vec::new(),
            shown_level: None,
            shown_status: Default::default(),
            save_at: None,
        };
        s.revert(core);
        s
    }

    /// Back to the outputs, rate and buffer in use.
    pub fn revert(&mut self, core: &Core) {
        self.outputs = core.prefs.effective_outputs();
        self.rate = core.prefs.rate();
        self.buffer = core.prefs.buffer_frames;
    }

    /// Outputs, rate or buffer differ from what plays now (names apply at once).
    pub fn audio_changed(&self, core: &Core) -> bool {
        let key = |l: &[OutputPref]| -> Vec<(String, u16, u16)> {
            l.iter()
                .map(|o| (o.device.clone(), o.first_channel, o.channels))
                .collect()
        };
        key(&self.outputs) != key(&core.prefs.effective_outputs())
            || self.rate != core.prefs.rate()
            || self.buffer != core.prefs.buffer_frames
    }

    /// The devices to offer: (id, shown name, channels), the system default first.
    fn devices(core: &Core) -> Vec<(String, String, u16)> {
        let default = core.devices.iter().find(|d| d.is_default);
        let mut list = vec![(
            String::new(),
            match default {
                Some(d) => format!("System Default ({})", d.name),
                None => "System Default".into(),
            },
            default.map_or(2, |d| d.max_channels.max(1)),
        )];
        list.extend(
            core.devices
                .iter()
                .map(|d| (d.name.clone(), d.name.clone(), d.max_channels.max(1))),
        );
        // A device set up here but not connected now keeps its row.
        for o in &core.prefs.effective_outputs() {
            if !list.iter().any(|d| d.0 == o.device) {
                list.push((
                    o.device.clone(),
                    format!("{} (not connected)", o.device),
                    o.first_channel + o.channels,
                ));
            }
        }
        list
    }

    pub fn show(&mut self, core: &Core) {
        let w = &self.win;
        let p = &core.prefs;
        let m = p.machine();
        w.set_appearance(appearance_text(p.appearance).into());
        w.set_bar_size(
            menu::BAR_SIZES
                .get(p.bar_size as usize)
                .copied()
                .unwrap_or("Small")
                .into(),
        );
        w.set_bar_position(bar_position_text(p.bar_top).into());
        w.set_tile_progress(
            menu::t(menu::TILE_PROGRESS[usize::from(p.tile_progress.min(2))]).into(),
        );
        w.set_mouse(mouse_text(m.mouse_mode).into());
        w.set_session_folder(core.session_root().display().to_string().into());

        if self.shown_level != Some(m.output_level_db) {
            self.shown_level = Some(m.output_level_db);
            w.set_level(m.output_level_db);
        }
        w.set_level_text(level_text(m.output_level_db).into());
        w.set_limiter(m.limiter);
        let devices = Self::devices(core);
        let in_use = p.effective_outputs();
        let rows: Vec<OutputRow> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let letter = bubble_project::output_letter(i);
                let device = devices
                    .iter()
                    .find(|d| d.0 == o.device)
                    .map_or_else(|| o.device.clone(), |d| d.1.clone());
                let applied = in_use.get(i).is_some_and(|u| {
                    (&u.device, u.first_channel, u.channels)
                        == (&o.device, o.first_channel, o.channels)
                });
                let mismatch = core
                    .rate_mismatches()
                    .into_iter()
                    .find(|(k, _, _)| *k == o.device);
                let note = if !applied {
                    "Not applied yet.".to_string()
                } else if let Some(e) = core
                    .output_errors
                    .iter()
                    .find(|e| e.starts_with(&format!("Output {letter}")))
                {
                    e.clone()
                } else if let Some((_, _, rate)) = mismatch {
                    // Bubble could not set it: the user must, or Windows converts again.
                    format!(
                        "Windows runs this device at {}. Set it to {} in Sound settings \
                         (the device's Properties ▸ Advanced ▸ Default format).",
                        bubble_core::view::khz(rate),
                        bubble_core::view::khz(core.prefs.rate())
                    )
                } else {
                    String::new()
                };
                OutputRow {
                    letter: letter.into(),
                    alias: o.alias.clone().into(),
                    device: device.into(),
                    channels: o.channel_text().into(),
                    note: note.into(),
                }
            })
            .collect();
        refresh(&mut self.shown_outputs, rows, |m| w.set_outputs(m));
        let dev_channels: Vec<(String, u16)> = devices.iter().map(|d| (d.0.clone(), d.2)).collect();
        w.set_can_add_output(
            self.outputs.len() < bubble_project::MAX_OUTPUTS
                && outputs::next_free(&self.outputs, &dev_channels).is_some(),
        );
        w.set_rate(rate_text(self.rate).into());
        w.set_buffer(menu::buffer_label(self.buffer).into());
        w.set_file_link(!p.no_file_link);
        w.set_audio_changed(self.audio_changed(core));
        w.set_audio_problem(core.audio_error.clone().unwrap_or_default().into());
        w.set_scanning(core.devices_scan.is_some());

        w.set_midi_on(!p.midi_disabled);
        w.set_all_ports(p.midi_inputs.is_empty());
        let ports: Vec<PortRow> = core
            .midi_ports
            .iter()
            .map(|n| PortRow {
                name: n.clone().into(),
                on: p.midi_inputs.is_empty() || p.midi_inputs.contains(n),
                active: core.midi.port_active_recently(n, 150),
            })
            .collect();
        refresh(&mut self.shown_ports, ports, |m| w.set_ports(m));
        self.show_status(core);
    }

    /// The choices for a pop-up button, set on the window for its native menu.
    pub fn choices(&mut self, field: &str, row: usize, core: &Core) {
        let p = &core.prefs;
        let m = p.machine();
        let mut list = Vec::new();
        self.picks.clear();
        let mut add = |pick: Pick, text: String, enabled: bool, checked: bool| {
            self.picks.push(pick);
            list.push((text, enabled, checked));
        };
        match field {
            "appearance" => {
                for (n, a) in menu::APPEARANCES {
                    add(Pick::Appearance(a), n.into(), true, p.appearance == a);
                }
            }
            "bar" => {
                for (i, n) in menu::BAR_SIZES.into_iter().enumerate() {
                    add(Pick::Bar(i as u8), n.into(), true, p.bar_size == i as u8);
                }
            }
            "tile-progress" => {
                for (i, n) in menu::TILE_PROGRESS.into_iter().enumerate() {
                    add(
                        Pick::TileProgress(i as u8),
                        menu::t(n),
                        true,
                        p.tile_progress == i as u8,
                    );
                }
            }
            "bar-position" => {
                for top in [false, true] {
                    add(
                        Pick::BarTop(top),
                        bar_position_text(top),
                        true,
                        p.bar_top == top,
                    );
                }
            }
            "mouse" => {
                for mm in [MouseMode::Toggle, MouseMode::LeftPlayRightStop] {
                    add(Pick::Mouse(mm), mouse_text(mm), true, m.mouse_mode == mm);
                }
            }
            "rate" => {
                for r in menu::SAMPLE_RATES {
                    add(Pick::Rate(r), rate_text(r), true, self.rate == r);
                }
            }
            "buffer" => {
                for b in menu::BUFFER_SIZES {
                    add(
                        Pick::Buffer(b),
                        menu::buffer_label(b),
                        true,
                        self.buffer == b,
                    );
                }
            }
            "device" => {
                let current = self.outputs.get(row).map(|o| o.device.clone());
                for (id, shown, n) in Self::devices(core) {
                    // A device with no channel left for this output is shown greyed.
                    let free = outputs::channel_choices(&self.outputs, row, &id, n)
                        .iter()
                        .any(|c| c.2);
                    let on = current.as_deref() == Some(id.as_str());
                    add(Pick::Device(row, id), shown, free || on, on);
                }
            }
            "channels" => {
                if let Some(o) = self.outputs.get(row).cloned() {
                    let n = Self::devices(core)
                        .into_iter()
                        .find(|d| d.0 == o.device)
                        .map_or(o.first_channel + o.channels, |d| d.2);
                    for (first, width, free) in
                        outputs::channel_choices(&self.outputs, row, &o.device, n)
                    {
                        let on = (first, width) == (o.first_channel, o.channels);
                        let mut text = outputs::channel_text(first, width);
                        if width == 1 {
                            text = format!("{text} (mono)");
                        }
                        add(Pick::Channels(row, first, width), text, free || on, on);
                    }
                }
            }
            _ => {}
        }
        self.win.set_popup_rows(rows(list));
    }

    /// A pop-up choice. Appearance, page bar and mouse are menu commands (the app runs
    /// them, `Some`); outputs, rate and buffer change the draft.
    pub fn chosen(&mut self, id: i32, core: &Core) -> Option<Cmd> {
        let pick = self.picks.get(usize::try_from(id).ok()?)?.clone();
        match pick {
            Pick::Appearance(a) => return Some(Cmd::Appearance(a)),
            Pick::Bar(b) => return Some(Cmd::BarSize(b)),
            Pick::BarTop(top) => return Some(Cmd::BarTop(top)),
            Pick::TileProgress(t) => return Some(Cmd::TileProgress(t)),
            Pick::Mouse(MouseMode::Toggle) => return Some(Cmd::MouseToggle),
            Pick::Mouse(MouseMode::LeftPlayRightStop) => return Some(Cmd::MouseLeftRight),
            Pick::Rate(r) => self.rate = r,
            Pick::Buffer(b) => self.buffer = b,
            Pick::Device(row, device) => {
                let n = Self::devices(core)
                    .into_iter()
                    .find(|d| d.0 == device)
                    .map_or(2, |d| d.2);
                let choices = outputs::channel_choices(&self.outputs, row, &device, n);
                // Keep the same width where it fits, else the first free stereo or mono.
                let width = self.outputs.get(row).map_or(2, |o| o.channels);
                let pick = choices
                    .iter()
                    .find(|c| c.2 && c.1 == width)
                    .or_else(|| choices.iter().find(|c| c.2));
                if let (Some(&(first, w, _)), Some(o)) = (pick, self.outputs.get_mut(row)) {
                    o.device = device;
                    o.first_channel = first;
                    o.channels = w;
                }
            }
            Pick::Channels(row, first, width) => {
                if let Some(o) = self.outputs.get_mut(row) {
                    o.first_channel = first;
                    o.channels = width;
                }
            }
        }
        None
    }

    pub fn alias(&mut self, row: usize, text: &str, core: &mut Core) {
        let Some(o) = self.outputs.get_mut(row) else {
            return;
        };
        o.alias = text.to_string();
        if let Some(s) = self.shown_outputs.get_mut(row) {
            s.alias = text.into();
        }
        // A name never needs audio restarted: it applies at once when this output is
        // the one in use at that letter.
        let mut in_use = core.prefs.effective_outputs();
        if let Some(u) = in_use.get_mut(row)
            && (&u.device, u.first_channel, u.channels) == (&o.device, o.first_channel, o.channels)
            && u.alias != o.alias
        {
            u.alias = o.alias.clone();
            core.prefs.outputs = in_use;
            core.save_prefs();
        }
    }

    pub fn level(&mut self, db: f32, core: &mut Core) {
        let db = (db * 2.0).round() / 2.0;
        self.shown_level = Some(db);
        core.prefs.output_level_db = Some(db);
        core.apply_machine();
        self.save_at = Some(Instant::now() + Duration::from_millis(400));
    }

    /// Saves the output level once the slider has rested.
    pub fn poll_save(&mut self, core: &mut Core) {
        if self.save_at.is_some_and(|t| Instant::now() >= t) {
            self.save_at = None;
            core.save_prefs();
        }
    }

    /// Buttons. Returns a menu command for the app to run, if any.
    pub fn action(&mut self, what: &str, row: usize, core: &mut Core) -> Option<Cmd> {
        match what {
            "add-output" => {
                let devs: Vec<(String, u16)> = Self::devices(core)
                    .into_iter()
                    .map(|d| (d.0, d.2))
                    .collect();
                if let Some(o) = outputs::next_free(&self.outputs, &devs) {
                    self.outputs.push(o);
                }
            }
            "remove-output" => {
                if row < self.outputs.len() {
                    self.outputs.remove(row);
                }
            }
            "rescan" => core.scan_devices(),
            "revert" => self.revert(core),
            "apply" => {
                core.prefs.sample_rate = self.rate;
                core.prefs.buffer_frames = self.buffer;
                // Letters stay as shown here (no sorting by device).
                core.set_outputs(self.outputs.clone(), true);
                self.revert(core);
            }
            "rescan-midi" => return Some(Cmd::RescanMidi),
            "status-toggle" => {
                core.prefs.audio_status_open = !core.prefs.audio_status_open;
                core.save_prefs();
            }
            "reopen" => core.open_audio(),
            "copy-status" => {
                let (summary, _, lines) = status(core, true);
                let text: Vec<String> = std::iter::once(format!("Audio status: {summary}"))
                    .chain(lines.iter().map(|l| format!("{}: {}", l.label, l.value)))
                    .collect();
                if let Ok(mut c) = arboard::Clipboard::new() {
                    let _ = c.set_text(text.join("\n"));
                }
            }
            _ => {}
        }
        None
    }

    /// Settings ▸ Audio's status section.
    fn show_status(&mut self, core: &Core) {
        let w = &self.win;
        let open = core.prefs.audio_status_open;
        // Closed, only the dropout counts are read for the summary.
        let (summary, bad, lines) = status(core, open);
        w.set_status_open(open);
        w.set_status_bad(bad);
        w.set_status_summary(summary.into());
        // Devices across the whole width; the rest in two columns.
        let (devices, rest): (Vec<_>, Vec<_>) = lines
            .into_iter()
            .partition(|l| l.label.starts_with("Output"));
        let half = rest.len().div_ceil(2);
        let right = rest[half..].to_vec();
        let left = rest[..half].to_vec();
        let new = (devices, left, right);
        if self.shown_status != new {
            w.set_status_devices(ModelRc::new(VecModel::from(new.0.clone())));
            w.set_status_left(ModelRc::new(VecModel::from(new.1.clone())));
            w.set_status_right(ModelRc::new(VecModel::from(new.2.clone())));
            self.shown_status = new;
        }
    }

    pub fn toggle(&self, what: &str, row: usize, core: &Core) -> Option<Cmd> {
        match what {
            "limiter" => Some(Cmd::Limiter),
            "file-link" => Some(Cmd::OpenBubbleFiles),
            "midi" => Some(Cmd::MidiEnabled),
            "all-ports" => {
                if core.prefs.midi_inputs.is_empty() {
                    // All off: keep every port listed now, so turning one off is next.
                    None
                } else {
                    Some(Cmd::MidiAllPorts)
                }
            }
            "port" => core.midi_ports.get(row).cloned().map(Cmd::MidiPort),
            _ => None,
        }
    }

    /// All Ports off: every port listed now stays on, picked one by one from here.
    pub fn ports_one_by_one(core: &mut Core) {
        core.prefs.midi_inputs = core.midi_ports.clone();
        core.reopen_midi();
    }
}

// ---------------------------------------------------------------------------------
// Session Settings
// ---------------------------------------------------------------------------------

#[derive(Clone)]
enum SessionPick {
    AudioFiles(bool),
    Fade(u32),
    Retrigger(Retrigger),
    Warning(f32),
    Duplicates(DuplicatePolicy),
    Scope(Scope),
    Typeface(Option<String>),
    MoveGroup(uuid::Uuid),
    MoveTo(String),
}

/// Outputs ▸ Move…: from which letter, which carts (0 all, 1 this page, 2 a group),
/// the group, and to which letter.
struct Move {
    from: String,
    scope: i32,
    group: Option<uuid::Uuid>,
    to: String,
}

/// Typefaces offered for tile labels, by family name (a name ending in "Bold" is
/// that family at bold weight). Sessions store these names.
#[cfg(target_os = "macos")]
const TYPEFACES: &[&str] = &[
    "Helvetica",
    "Helvetica Bold",
    "Helvetica Neue",
    "Helvetica Neue Bold",
    "Avenir Next",
    "Avenir Next Demi Bold",
    "Futura",
    "Gill Sans",
    "Arial",
    "Arial Bold",
    "Verdana",
    "Menlo",
];
#[cfg(not(target_os = "macos"))]
const TYPEFACES: &[&str] = &[
    "Segoe UI",
    "Segoe UI Bold",
    "Arial",
    "Arial Bold",
    "Calibri",
    "Tahoma",
    "Verdana",
    "Consolas",
];

/// Ending warnings offered (seconds; 0 = off).
const WARNINGS: [f32; 9] = [0.0, 1.0, 2.0, 3.0, 5.0, 10.0, 15.0, 20.0, 30.0];

fn warning_text(s: f32) -> String {
    if s <= 0.0 {
        "Off".into()
    } else if (s - s.round()).abs() < 0.05 {
        format!("{s:.0} s before the end")
    } else {
        format!("{s:.1} s before the end")
    }
}

fn duplicates_text(d: DuplicatePolicy) -> String {
    match d {
        DuplicatePolicy::Ganged => "Ganged".into(),
        DuplicatePolicy::Overlapped => "Overlapped".into(),
    }
}

fn scope_text(s: Scope) -> String {
    match s {
        Scope::Global => "Global".into(),
        Scope::CurrentPage => name("Current Page", "Current page"),
    }
}

fn audio_files_text(wav: bool) -> String {
    if wav {
        "Convert to WAV".into()
    } else {
        "Keep as Imported".into()
    }
}

fn percent(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

pub struct SessionSettings {
    pub win: SessionSettingsWindow,
    /// As `Settings::tallest`.
    pub tallest: f32,
    picks: Vec<SessionPick>,
    shown_name: Option<String>,
    shown_label: Option<f32>,
    shown_small: Option<f32>,
    shown_groups: Vec<GroupRow>,
    shown_pages: Vec<PageRow>,
    shown_uses: Vec<OutputUse>,
    moving: Option<Move>,
    /// This computer's outputs (from Settings ▸ Audio), for where each letter plays.
    outputs: Vec<OutputPref>,
}

/// The carts (not control buttons) that play audio.
fn audio_carts(
    p: &bubble_project::Project,
) -> impl Iterator<Item = (usize, &bubble_project::Cart)> {
    p.pages.iter().enumerate().flat_map(|(pi, page)| {
        page.cells
            .iter()
            .map(move |c| (pi, &c.cart))
            .filter(|(_, c)| c.control.is_none() && c.asset.is_some())
    })
}

fn carts_text(n: usize) -> String {
    if n == 1 {
        "1 cart".into()
    } else {
        format!("{n} carts")
    }
}

impl SessionSettings {
    pub fn new(win: SessionSettingsWindow) -> Self {
        SessionSettings {
            tallest: 0.0,
            win,
            picks: Vec::new(),
            shown_name: None,
            shown_label: None,
            shown_small: None,
            shown_groups: Vec::new(),
            shown_pages: Vec::new(),
            shown_uses: Vec::new(),
            moving: None,
            outputs: Vec::new(),
        }
    }

    /// The Outputs tab and Move….
    fn show_outputs(&mut self, snap: &Snapshot) {
        let p = &snap.project;
        let mut letters: Vec<String> = audio_carts(p).map(|(_, c)| c.output.clone()).collect();
        letters.sort_by_key(|l| bubble_project::output_index(l).unwrap_or(usize::MAX));
        letters.dedup();
        let uses: Vec<OutputUse> = letters
            .iter()
            .map(|l| {
                let n = audio_carts(p).filter(|(_, c)| &c.output == l).count();
                let here = bubble_project::output_index(l).and_then(|i| self.outputs.get(i));
                OutputUse {
                    letter: l.clone().into(),
                    carts: carts_text(n).into(),
                    device: match here {
                        Some(o) if o.device.is_empty() => {
                            name("System Default", "System default").into()
                        }
                        Some(o) => o.device.clone().into(),
                        None => "Not set up on this computer".into(),
                    },
                    channels: here
                        .map(|o| {
                            if o.channels == 1 {
                                format!("Channel {}", o.first_channel + 1)
                            } else {
                                format!(
                                    "Channels {}–{}",
                                    o.first_channel + 1,
                                    o.first_channel + o.channels
                                )
                            }
                        })
                        .unwrap_or_default()
                        .into(),
                    missing: here.is_none(),
                    note: if n == 1 {
                        "This cart plays on Output A instead.".to_string()
                    } else {
                        format!("These {n} carts play on Output A instead.")
                    }
                    .into(),
                }
            })
            .collect();
        refresh(&mut self.shown_uses, uses, |m| self.win.set_output_uses(m));

        let w = &self.win;
        let Some(m) = &self.moving else {
            w.set_move_open(false);
            return;
        };
        let from = |c: &&bubble_project::Cart| c.output == m.from;
        let all = audio_carts(p).filter(|(_, c)| from(c)).count();
        let page = audio_carts(p)
            .filter(|(pi, c)| *pi == snap.page && from(c))
            .count();
        let group = m.group.map_or(0, |g| {
            audio_carts(p)
                .filter(|(_, c)| from(c) && c.group == Some(g))
                .count()
        });
        let n = match m.scope {
            1 => page,
            2 => group,
            _ => all,
        };
        let gi = m
            .group
            .and_then(|g| p.groups.iter().position(|x| x.id == g));
        w.set_move_open(true);
        w.set_move_from(m.from.clone().into());
        w.set_move_scope(m.scope);
        w.set_move_all(carts_text(all).into());
        w.set_move_page(
            p.pages
                .get(snap.page)
                .map(|x| x.name.clone())
                .unwrap_or_default()
                .into(),
        );
        w.set_move_page_count(carts_text(page).into());
        w.set_has_groups(!p.groups.is_empty());
        w.set_move_group(
            gi.map_or("None".into(), |i| p.groups[i].display_name(i))
                .into(),
        );
        w.set_move_group_count(
            if m.group.is_some() {
                carts_text(group)
            } else {
                String::new()
            }
            .into(),
        );
        w.set_move_to(
            if m.to.is_empty() {
                String::new()
            } else {
                format!("Output {}", m.to)
            }
            .into(),
        );
        w.set_move_summary(
            if m.to.is_empty() {
                "There is no other output to move them to.".into()
            } else if n == 0 {
                "No carts to move.".into()
            } else if m.scope == 0 {
                format!("{} will move from {} to {}.", carts_text(n), m.from, m.to)
            } else {
                format!(
                    "{} will move from {} to {}. Other carts on Output {} stay there.",
                    carts_text(n),
                    m.from,
                    m.to,
                    m.from
                )
            }
            .into(),
        );
        let carts = if n == 1 {
            name("Cart", "cart")
        } else {
            name("Carts", "carts")
        };
        w.set_move_button(format!("Move {n} {carts}").into());
        w.set_move_enabled(n > 0 && !m.to.is_empty() && m.to != m.from);
    }

    /// Letters to move to: this computer's outputs and those in use, but not `from`.
    fn targets(&self, snap: &Snapshot, from: &str) -> Vec<String> {
        let mut v: Vec<String> = (0..self.outputs.len())
            .map(bubble_project::output_letter)
            .chain(audio_carts(&snap.project).map(|(_, c)| c.output.clone()))
            .filter(|l| l != from)
            .collect();
        v.sort_by_key(|l| bubble_project::output_index(l).unwrap_or(usize::MAX));
        v.dedup();
        v
    }

    pub fn set_outputs(&mut self, outputs: Vec<OutputPref>) {
        self.outputs = outputs;
    }

    pub fn show(&mut self, snap: &Snapshot) {
        self.show_outputs(snap);
        let w = &self.win;
        let p = &snap.project;
        let s = &p.settings;
        w.set_window_title(format!("Session Settings - {}", p.name).into());
        if self.shown_name.as_deref() != Some(p.name.as_str()) {
            self.shown_name = Some(p.name.clone());
            w.set_name_text(p.name.clone().into());
        }
        w.set_audio_files(audio_files_text(s.convert_to_wav).into());
        w.set_fade(crate::model::fade_text(s.default_fade_out_ms).into());
        w.set_retrigger(crate::cartsetup::retrigger_text(s.default_retrigger).into());
        w.set_warning(warning_text(s.ending_warning_s).into());
        w.set_duplicates(duplicates_text(s.duplicate_binding_policy).into());
        w.set_scope(scope_text(s.binding_scope).into());

        let d = &s.display;
        w.set_typeface(d.font.clone().unwrap_or_else(|| "System".into()).into());
        if self.shown_label != Some(d.label_size) {
            self.shown_label = Some(d.label_size);
            w.set_label_size(d.label_size);
        }
        if self.shown_small != Some(d.small_size) {
            self.shown_small = Some(d.small_size);
            w.set_small_size(d.small_size);
        }
        w.set_label_size_text(percent(d.label_size).into());
        w.set_small_size_text(percent(d.small_size).into());
        let align = |i: usize| i as i32;
        w.set_h_align(align(d.h_align as usize));
        w.set_v_align(align(d.v_align as usize));

        let groups: Vec<GroupRow> = p
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let n = p
                    .carts()
                    .filter(|(_, c)| c.cart.group == Some(g.id))
                    .count();
                GroupRow {
                    label: format!("Group {}", i + 1).into(),
                    name: g.name.clone().into(),
                    carts: match n {
                        0 => String::new(),
                        1 => "1 cart".into(),
                        n => format!("{n} carts"),
                    }
                    .into(),
                }
            })
            .collect();
        refresh(&mut self.shown_groups, groups, |m| w.set_groups(m));
        let pages: Vec<PageRow> = p
            .pages
            .iter()
            .enumerate()
            .map(|(i, pg)| {
                let first = p.cell_number(i, 0, 0);
                let last = first + (pg.rows as u32 * pg.cols as u32).saturating_sub(1);
                PageRow {
                    name: pg.name.clone().into(),
                    rows: pg.rows as i32,
                    cols: pg.cols as i32,
                    fewer_rows: pg.rows > 1 && menu::grid_fits(snap, i, pg.rows - 1, pg.cols),
                    fewer_cols: pg.cols > 1 && menu::grid_fits(snap, i, pg.rows, pg.cols - 1),
                    numbers: format!("{first}–{last}").into(),
                }
            })
            .collect();
        refresh(&mut self.shown_pages, pages, |m| w.set_pages(m));
        w.set_can_delete_page(p.pages.len() > 1);
    }

    pub fn choices(&mut self, field: &str, _row: usize, snap: &Snapshot) {
        let s = &snap.project.settings;
        // Move…'s choices of output, and the one chosen.
        let (targets, cur_to) = self
            .moving
            .as_ref()
            .map(|m| (self.targets(snap, &m.from), m.to.clone()))
            .unwrap_or_default();
        let cur_group = self.moving.as_ref().and_then(|m| m.group);
        let mut list = Vec::new();
        self.picks.clear();
        let mut add = |pick: SessionPick, text: String, enabled: bool, checked: bool| {
            self.picks.push(pick);
            list.push((text, enabled, checked));
        };
        match field {
            "audio-files" => {
                for wav in [false, true] {
                    add(
                        SessionPick::AudioFiles(wav),
                        audio_files_text(wav),
                        true,
                        s.convert_to_wav == wav,
                    );
                }
            }
            "fade" => {
                let mut all: Vec<u32> = crate::model::FADE_PRESETS_MS.to_vec();
                if !all.contains(&s.default_fade_out_ms) {
                    all.push(s.default_fade_out_ms);
                    all.sort();
                }
                for ms in all {
                    add(
                        SessionPick::Fade(ms),
                        crate::model::fade_text(ms),
                        true,
                        ms == s.default_fade_out_ms,
                    );
                }
            }
            "retrigger" => {
                for r in [
                    Retrigger::Restart,
                    Retrigger::ToggleStop,
                    Retrigger::Overlap,
                    Retrigger::Ignore,
                ] {
                    add(
                        SessionPick::Retrigger(r),
                        crate::cartsetup::retrigger_text(r).into(),
                        true,
                        s.default_retrigger == r,
                    );
                }
            }
            "warning" => {
                let mut all = WARNINGS.to_vec();
                if !all.iter().any(|w| (w - s.ending_warning_s).abs() < 0.05) {
                    all.push(s.ending_warning_s);
                    all.sort_by(f32::total_cmp);
                }
                for w in all {
                    add(
                        SessionPick::Warning(w),
                        warning_text(w),
                        true,
                        (w - s.ending_warning_s).abs() < 0.05,
                    );
                }
            }
            "duplicates" => {
                for d in [DuplicatePolicy::Ganged, DuplicatePolicy::Overlapped] {
                    add(
                        SessionPick::Duplicates(d),
                        duplicates_text(d),
                        true,
                        s.duplicate_binding_policy == d,
                    );
                }
            }
            "scope" => {
                for sc in [Scope::Global, Scope::CurrentPage] {
                    add(
                        SessionPick::Scope(sc),
                        scope_text(sc),
                        true,
                        s.binding_scope == sc,
                    );
                }
            }
            "move-group" => {
                for (i, g) in snap.project.groups.iter().enumerate() {
                    let on = cur_group == Some(g.id);
                    add(SessionPick::MoveGroup(g.id), g.display_name(i), true, on);
                }
            }
            "move-to" => {
                for l in targets {
                    let on = l == cur_to;
                    add(
                        SessionPick::MoveTo(l.clone()),
                        format!("Output {l}"),
                        true,
                        on,
                    );
                }
            }
            "typeface" => {
                let current = s.display.font.clone();
                add(
                    SessionPick::Typeface(None),
                    "System".into(),
                    true,
                    current.is_none(),
                );
                for f in TYPEFACES {
                    add(
                        SessionPick::Typeface(Some(f.to_string())),
                        f.to_string(),
                        true,
                        current.as_deref() == Some(*f),
                    );
                }
                if let Some(c) = current.filter(|c| !TYPEFACES.contains(&c.as_str())) {
                    add(SessionPick::Typeface(Some(c.clone())), c, true, true);
                }
            }
            _ => {}
        }
        self.win.set_popup_rows(rows(list));
    }

    pub fn chosen(&mut self, id: i32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let Some(pick) = usize::try_from(id)
            .ok()
            .and_then(|i| self.picks.get(i))
            .cloned()
        else {
            return;
        };
        match &pick {
            SessionPick::MoveGroup(g) => {
                if let Some(m) = &mut self.moving {
                    m.group = Some(*g);
                    m.scope = 2;
                }
                return;
            }
            SessionPick::MoveTo(l) => {
                if let Some(m) = &mut self.moving {
                    m.to = l.clone();
                }
                return;
            }
            _ => {}
        }
        let mut st = snap.project.settings.clone();
        match pick {
            SessionPick::MoveGroup(_) | SessionPick::MoveTo(_) => {}
            SessionPick::AudioFiles(w) => st.convert_to_wav = w,
            SessionPick::Fade(ms) => st.default_fade_out_ms = ms,
            SessionPick::Retrigger(r) => st.default_retrigger = r,
            SessionPick::Warning(w) => st.ending_warning_s = w,
            SessionPick::Duplicates(d) => st.duplicate_binding_policy = d,
            SessionPick::Scope(s) => st.binding_scope = s,
            SessionPick::Typeface(f) => st.display.font = f,
        }
        if st != snap.project.settings {
            intents.push(Intent::Settings(st));
        }
    }

    pub fn edited(
        &mut self,
        field: &str,
        row: usize,
        text: &str,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        match field {
            "name" => {
                self.shown_name = Some(text.to_string());
                intents.push(Intent::RenameProject(text.to_string()));
            }
            "group" => {
                let mut groups = snap.project.groups.clone();
                if let Some(g) = groups.get_mut(row) {
                    g.name = text.to_string();
                    if let Some(s) = self.shown_groups.get_mut(row) {
                        s.name = text.into();
                    }
                    intents.push(Intent::SetGroups(groups));
                }
            }
            "page" => {
                if let Some(s) = self.shown_pages.get_mut(row) {
                    s.name = text.into();
                }
                intents.push(Intent::RenamePage(row, text.to_string()));
            }
            _ => {}
        }
    }

    pub fn slider(&mut self, field: &str, v: f32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let v = ((v * 20.0).round() / 20.0).clamp(0.5, 3.0);
        let mut st = snap.project.settings.clone();
        match field {
            "label-size" => {
                st.display.label_size = v;
                self.shown_label = Some(v);
            }
            "small-size" => {
                st.display.small_size = v;
                self.shown_small = Some(v);
            }
            _ => return,
        }
        if st != snap.project.settings {
            intents.push(Intent::Settings(st));
        }
    }

    pub fn reset(&mut self, field: &str, snap: &Snapshot, intents: &mut Vec<Intent>) {
        self.slider(field, 1.0, snap, intents);
        // The knob moves back too.
        match field {
            "label-size" => self.win.set_label_size(1.0),
            "small-size" => self.win.set_small_size(1.0),
            _ => {}
        }
    }

    pub fn chose(&mut self, field: &str, i: i32, snap: &Snapshot, intents: &mut Vec<Intent>) {
        use bubble_project::{HAlign, VAlign};
        let mut st = snap.project.settings.clone();
        match field {
            "h-align" => {
                st.display.h_align =
                    [HAlign::Left, HAlign::Center, HAlign::Right][i.clamp(0, 2) as usize]
            }
            "v-align" => {
                st.display.v_align =
                    [VAlign::Top, VAlign::Center, VAlign::Bottom][i.clamp(0, 2) as usize]
            }
            _ => return,
        }
        if st != snap.project.settings {
            intents.push(Intent::Settings(st));
        }
    }

    /// Buttons that are intents; Delete Page… asks first, so the app handles it.
    pub fn action(&mut self, what: &str, row: usize, snap: &Snapshot, intents: &mut Vec<Intent>) {
        match what {
            "move" => {
                if let Some(u) = self.shown_uses.get(row) {
                    let from = u.letter.to_string();
                    let to = self
                        .targets(snap, &from)
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    self.moving = Some(Move {
                        from,
                        scope: 0,
                        group: snap.project.groups.first().map(|g| g.id),
                        to,
                    });
                }
            }
            "move-scope" => {
                if let Some(m) = &mut self.moving {
                    m.scope = row as i32;
                }
            }
            "move-cancel" => self.moving = None,
            "move-confirm" => {
                if let Some(m) = self.moving.take() {
                    let scope = match (m.scope, m.group) {
                        (1, _) => bubble_control::ReassignScope::Page(snap.page),
                        (2, Some(g)) => bubble_control::ReassignScope::Group(g),
                        _ => bubble_control::ReassignScope::All,
                    };
                    intents.push(Intent::ReassignOutputs(Some(m.from), m.to, scope));
                }
            }
            "add-group" => intents.push(Intent::AddGroup),
            "remove-group" => {
                let mut groups = snap.project.groups.clone();
                if row < groups.len() {
                    groups.remove(row);
                    intents.push(Intent::SetGroups(groups));
                }
            }
            "add-page" => intents.push(Intent::AddPage),
            "rows-up" | "rows-down" | "cols-up" | "cols-down" => {
                if let Some(pg) = snap.project.pages.get(row) {
                    let max = bubble_project::validate::MAX_GRID;
                    let step = |v: u16, up: bool| {
                        if up {
                            (v + 1).min(max)
                        } else {
                            v.saturating_sub(1).max(1)
                        }
                    };
                    let (r, c) = match what {
                        "rows-up" => (step(pg.rows, true), pg.cols),
                        "rows-down" => (step(pg.rows, false), pg.cols),
                        "cols-up" => (pg.rows, step(pg.cols, true)),
                        _ => (pg.rows, step(pg.cols, false)),
                    };
                    if (r, c) != (pg.rows, pg.cols) && menu::grid_fits(snap, row, r, c) {
                        intents.push(Intent::ResizePage(row, r, c));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Copies the main window's look onto a tool window, which has its own `Theme`.
macro_rules! share_theme {
    ($ui:expr, $win:expr) => {{
        let theme = $ui.global::<crate::Theme>();
        let t = $win.global::<crate::Theme>();
        if t.get_mac() != theme.get_mac() {
            t.set_mac(theme.get_mac());
        }
        if t.get_ui_font() != theme.get_ui_font() {
            t.set_ui_font(theme.get_ui_font());
        }
        if t.get_appearance() != theme.get_appearance() {
            t.set_appearance(theme.get_appearance());
        }
        if t.get_system_dark() != theme.get_system_dark() {
            t.set_system_dark(theme.get_system_dark());
        }
        if t.get_accent_base() != theme.get_accent_base() {
            t.set_accent_base(theme.get_accent_base());
            t.set_accent_light(theme.get_accent_light());
            t.set_accent_deep(theme.get_accent_deep());
        }
    }};
}
pub(crate) use share_theme;

/// Opens a native menu of the window's pop-up rows at (x, y), outside the handler
/// that asked (see `window::on_main`).
pub fn open_menu<C: ComponentHandle + 'static>(win: &C, x: f32, y: f32, open: fn(&C)) {
    let w = win.as_weak();
    crate::window::on_main(move || {
        if let Some(win) = w.upgrade() {
            crate::window::run_menu(x, y, || open(&win));
        }
    });
}
