//! Per-machine preferences, stored as JSON in the
//! platform config directory. Never part of a project.

use std::path::PathBuf;

use bubble_control::Machine;
use bubble_project::MouseMode;
use serde::{Deserialize, Serialize};

/// One enabled output, in letter order (the first is Output A).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPref {
    /// Device name as listed; empty = the system default device.
    pub device: String,
    /// First channel, 0-based.
    pub first_channel: u16,
    /// 1 (mono), 2 (stereo) or 6 (5.1).
    pub channels: u16,
    /// Optional name shown after the letter ("Output A - PA").
    #[serde(default)]
    pub alias: String,
}

impl OutputPref {
    pub fn system_default() -> Self {
        OutputPref {
            device: String::new(),
            first_channel: 0,
            channels: 2,
            alias: String::new(),
        }
    }

    /// "Ch 1", "Ch 1–2" or "Ch 1–6 (5.1)".
    pub fn channel_text(&self) -> String {
        let a = self.first_channel + 1;
        match self.channels {
            1 => format!("Ch {a}"),
            6 => format!("Ch {a}–{} (5.1)", a + 5),
            n => format!("Ch {a}–{}", a + n - 1),
        }
    }
}

/// Light or dark interface, or whatever the system uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// The main window's last place: outer position and inner size in logical points
/// (zoom-independent), and whether it was maximised. When maximised, the position and
/// size are those it returns to when un-maximised.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowPlace {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default)]
    pub maximized: bool,
}

impl WindowPlace {
    /// Plausible enough to restore (not a size or place from a broken save).
    pub fn usable(&self) -> bool {
        let ok = |v: f32| v.is_finite() && v.abs() < 20_000.0;
        ok(self.x) && ok(self.y) && ok(self.w) && ok(self.h) && self.w >= 640.0 && self.h >= 420.0
    }
}

/// Default engine rate: 48 kHz (broadcast standard).
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Output device name; `None` = system default.
    pub audio_device: Option<String>,
    /// Fixed callback size in frames; `None` = backend default.
    pub buffer_frames: Option<u32>,
    /// MIDI input port names to open; empty = all ports.
    pub midi_inputs: Vec<String>,
    /// Disable MIDI input entirely.
    pub midi_disabled: bool,
    pub last_project: Option<PathBuf>,
    pub recent_projects: Vec<PathBuf>,
    pub ui_scale: Option<f32>,
    /// Bottom page/stop bar height: 0 small, 1 medium, 2 large (touch screens).
    pub bar_size: u8,
    /// The page bar at the top of the window instead of the bottom (Bubble Slint).
    pub bar_top: bool,
    /// Enabled outputs in letter order; empty = the system default device as Output A.
    pub outputs: Vec<OutputPref>,
    /// Engine sample rate for every output device; 0 = 48 kHz.
    pub sample_rate: u32,
    /// The outputs were reordered by hand (otherwise device/channel order).
    pub outputs_custom_order: bool,
    /// Folder new sessions are created in (default ~/Bubble).
    pub session_root: Option<PathBuf>,
    /// Light, dark, or follow the system.
    pub appearance: Appearance,
    /// Where the main window was, restored at launch.
    pub window: Option<WindowPlace>,
    /// Attenuation of everything Bubble plays on this machine, in dB (`None` = −10 dB).
    pub output_level_db: Option<f32>,
    /// The output safety limiter (`None` = on).
    pub limiter: Option<bool>,
    /// What a click on a cart does (`None` = click toggles play/stop).
    pub mouse_mode: Option<MouseMode>,
}

fn dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble").map(|d| d.config_dir().to_path_buf())
}

pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("preferences.json"))
}

impl Prefs {
    pub fn load() -> Prefs {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = path() else { return Ok(()) };
        let json = serde_json::to_string_pretty(self).expect("prefs serialise");
        bubble_project::io::write_atomic(&p, json.as_bytes(), false)
    }

    /// Outputs in letter order (never empty).
    pub fn effective_outputs(&self) -> Vec<OutputPref> {
        if self.outputs.is_empty() {
            let mut o = OutputPref::system_default();
            if let Some(d) = &self.audio_device {
                o.device = d.clone();
            }
            vec![o]
        } else {
            self.outputs.clone()
        }
    }

    pub fn rate(&self) -> u32 {
        // 48 kHz, or 44.1 kHz; anything else (an older 96 kHz setting) is the default.
        if self.sample_rate == 44_100 {
            44_100
        } else {
            DEFAULT_SAMPLE_RATE
        }
    }

    /// Output level, limiter and mouse mode, as the controller takes them.
    pub fn machine(&self) -> Machine {
        let d = Machine::default();
        Machine {
            output_level_db: self.output_level_db.unwrap_or(d.output_level_db),
            limiter: self.limiter.unwrap_or(d.limiter),
            mouse_mode: self.mouse_mode.unwrap_or(d.mouse_mode),
        }
    }

    /// Sessions saved before output level, limiter and mouse mode moved to the
    /// machine still carry them: the first session opened sets what this machine has
    /// not set yet, so nothing changes for whoever was using them. True if any were taken.
    pub fn adopt_from_session(&mut self, settings: &bubble_project::ProjectSettings) -> bool {
        let x = &settings.extra;
        let mut took = false;
        if self.output_level_db.is_none()
            && let Some(db) = x.get("master_gain_db").and_then(|v| v.as_f64())
        {
            self.output_level_db = Some((db as f32).clamp(-60.0, 12.0));
            took = true;
        }
        if self.limiter.is_none()
            && let Some(on) = x.get("limiter").and_then(|v| v.as_bool())
        {
            self.limiter = Some(on);
            took = true;
        }
        if self.mouse_mode.is_none()
            && let Some(m) = x
                .get("mouse_mode")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            self.mouse_mode = Some(m);
            took = true;
        }
        took
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent_projects.retain(|x| *x != p);
        self.recent_projects.insert(0, p.clone());
        self.recent_projects.truncate(10);
        self.last_project = Some(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_session_sets_only_what_the_machine_has_not() {
        let s: bubble_project::ProjectSettings = serde_json::from_str(
            r#"{"master_gain_db": 0.0, "limiter": false, "mouse_mode": "left_play_right_stop"}"#,
        )
        .unwrap();
        let mut p = Prefs {
            limiter: Some(true),
            ..Default::default()
        };
        assert!(p.adopt_from_session(&s));
        let m = p.machine();
        assert_eq!(m.output_level_db, 0.0);
        assert!(m.limiter, "already set on this machine");
        assert_eq!(m.mouse_mode, MouseMode::LeftPlayRightStop);
        // Once set, a session no longer changes them.
        assert!(!p.adopt_from_session(&s));
    }

    #[test]
    fn a_new_machine_defaults_to_minus_10_db_limiter_on() {
        let m = Prefs::default().machine();
        assert_eq!(m.output_level_db, -10.0);
        assert!(m.limiter);
        assert_eq!(m.mouse_mode, MouseMode::Toggle);
    }
}
