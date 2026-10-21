//! The application menus as data: every command, its label, shortcut and state.
//! Each interface shows this model its own way (native or in-window) and runs the
//! [`Cmd`]s chosen from it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use uuid::Uuid;

use crate::app::{Core, Snapshot};
use crate::prefs::Appearance;

/// Output level presets offered in the Session menu (dB).
pub const OUTPUT_LEVELS: [i32; 7] = [0, -3, -6, -10, -12, -15, -20];

/// Everything a menu item can ask the app to do.
#[derive(Clone, Debug, PartialEq, Hash)]
pub enum Cmd {
    New,
    Open,
    OpenRecent(usize),
    Save,
    SaveAs,
    Relink,
    ReloadAudio,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PasteAttributes,
    ClearCart,
    AudioSetup,
    PlaySelected,
    ToggleEditMode,
    AddPage,
    RemovePage,
    StopAll,
    StopPage,
    StopAllNow,
    /// Audio ▸ Outputs… window.
    Outputs,
    ReassignOutputs,
    /// Engine sample rate for every output device.
    SampleRate(u32),
    BufferFrames(Option<u32>),
    MidiEnabled,
    MidiAllPorts,
    MidiPort(String),
    RescanMidi,
    MouseToggle,
    MouseLeftRight,
    DuplicatesGanged,
    DuplicatesOverlapped,
    ScopeGlobal,
    ScopePage,
    Limiter,
    ShowLog,
    ShowDiagnostics,
    /// Shows Bubble's log file (start-up and audio events) in the file manager.
    ShowLogs,
    Zoom(u32),
    /// Global output attenuation preset in dB.
    OutputLevel(i32),
    ProjectSettings,
    RevealSession,
    CopyPage,
    PastePage,
    DuplicateCart,
    PageSettings,
    SpeedBar,
    /// Height of the bottom page/stop bar (0 small, 1 medium, 2 large).
    BarSize(u8),
    /// Interface appearance: follow the system, light or dark.
    Appearance(Appearance),
    Quit,
}

#[derive(Clone, Debug, Hash)]
pub enum Item {
    Action {
        label: String,
        cmd: Cmd,
        /// Accelerator in muda syntax, e.g. "CmdOrCtrl+S".
        accel: Option<&'static str>,
        checked: Option<bool>,
        enabled: bool,
    },
    Separator,
    Sub {
        label: String,
        items: Vec<Item>,
    },
}

impl Item {
    pub fn action(label: impl Into<String>, cmd: Cmd) -> Item {
        Item::Action {
            label: label.into(),
            cmd,
            accel: None,
            checked: None,
            enabled: true,
        }
    }

    pub fn accel(mut self, a: &'static str) -> Item {
        if let Item::Action { accel, .. } = &mut self {
            *accel = Some(a);
        }
        self
    }

    pub fn check(mut self, on: bool) -> Item {
        if let Item::Action { checked, .. } = &mut self {
            *checked = Some(on);
        }
        self
    }

    pub fn enabled(mut self, on: bool) -> Item {
        if let Item::Action { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    pub fn sub(label: impl Into<String>, items: Vec<Item>) -> Item {
        Item::Sub {
            label: label.into(),
            items,
        }
    }
}

#[derive(Clone, Debug, Hash)]
pub struct Top {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, Hash)]
pub struct Menu {
    pub tops: Vec<Top>,
}

impl Menu {
    /// Changes whenever anything shown in the menus changes.
    pub fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// The interface state the menus depend on.
pub struct MenuState {
    pub edit_mode: bool,
    /// The selected cart (if it still exists).
    pub selected: Option<Uuid>,
    /// Where Paste would go.
    pub paste_target: Option<(usize, u16, u16)>,
}

/// The menus for the current state.
pub fn build(core: &Core, snap: &Snapshot, st: &MenuState) -> Menu {
    let s = &snap.project.settings;
    let has_sel = st.selected.is_some();
    let recent: Vec<Item> = core
        .prefs
        .recent_projects
        .iter()
        .enumerate()
        .map(|(i, p)| Item::action(p.display().to_string(), Cmd::OpenRecent(i)))
        .collect();
    let mut file = vec![
        Item::action("New Session…", Cmd::New).accel("CmdOrCtrl+N"),
        Item::action("Open…", Cmd::Open).accel("CmdOrCtrl+O"),
    ];
    if !recent.is_empty() {
        file.push(Item::sub("Open Recent", recent));
    }
    file.extend([
        Item::Separator,
        Item::action("Save", Cmd::Save).accel("CmdOrCtrl+S"),
        Item::action("Save Session As…", Cmd::SaveAs).accel("CmdOrCtrl+Shift+S"),
        Item::Separator,
        Item::action(
            if cfg!(target_os = "macos") {
                "Show Session in Finder"
            } else {
                "Show Session Folder"
            },
            Cmd::RevealSession,
        )
        .enabled(snap.path.is_some()),
        Item::action("Relink Missing Files…", Cmd::Relink),
        Item::action("Reload All Audio", Cmd::ReloadAudio),
    ]);
    #[cfg(not(target_os = "macos"))]
    file.extend([
        Item::Separator,
        Item::action("Quit", Cmd::Quit).accel("CmdOrCtrl+Q"),
    ]);

    let edit = vec![
        Item::action("Undo", Cmd::Undo)
            .accel("CmdOrCtrl+Z")
            .enabled(snap.can_undo),
        Item::action("Redo", Cmd::Redo)
            .accel("CmdOrCtrl+Shift+Z")
            .enabled(snap.can_redo),
        Item::Separator,
        Item::action("Cut Cart", Cmd::Cut)
            .accel("CmdOrCtrl+X")
            .enabled(has_sel),
        Item::action("Copy Cart", Cmd::Copy)
            .accel("CmdOrCtrl+C")
            .enabled(has_sel),
        Item::action("Paste Cart", Cmd::Paste)
            .accel("CmdOrCtrl+V")
            .enabled(core.clipboard.is_some() && st.paste_target.is_some()),
        Item::action("Paste Attributes…", Cmd::PasteAttributes)
            .accel("CmdOrCtrl+Shift+V")
            .enabled(core.clipboard.is_some() && has_sel),
        Item::action("Duplicate Cart", Cmd::DuplicateCart)
            .accel("CmdOrCtrl+D")
            .enabled(has_sel),
        Item::action("Clear Cart", Cmd::ClearCart).enabled(has_sel),
        Item::Separator,
        Item::action("Copy Page", Cmd::CopyPage),
        Item::action("Paste Page", Cmd::PastePage).enabled(core.page_clip.is_some()),
        Item::Separator,
        Item::action("Edit Mode", Cmd::ToggleEditMode)
            .accel("CmdOrCtrl+E")
            .check(st.edit_mode),
    ];
    let level = s.master_gain_db;
    let mut levels: Vec<Item> = OUTPUT_LEVELS
        .iter()
        .map(|db| {
            let label = if *db == 0 {
                "0 dB (no attenuation)".to_string()
            } else if *db == bubble_project::DEFAULT_MASTER_GAIN_DB as i32 {
                format!("{db} dB (default)")
            } else {
                format!("{db} dB")
            };
            Item::action(label, Cmd::OutputLevel(*db)).check((level - *db as f32).abs() < 0.05)
        })
        .collect();
    let custom = !OUTPUT_LEVELS
        .iter()
        .any(|db| (level - *db as f32).abs() < 0.05);
    levels.extend([
        Item::Separator,
        Item::action(
            if custom {
                format!("Custom ({level:.1} dB)…")
            } else {
                "Custom…".to_string()
            },
            Cmd::ProjectSettings,
        )
        .check(custom),
    ]);

    let cart = vec![
        Item::action("Cart Setup…", Cmd::AudioSetup)
            .accel("CmdOrCtrl+I")
            .enabled(has_sel),
        Item::action("Speed Bar…", Cmd::SpeedBar).enabled(has_sel),
        Item::action("Play Selected Cart", Cmd::PlaySelected).enabled(has_sel),
    ];

    let rate = core.prefs.rate();
    let rates: Vec<Item> = [44_100u32, 48_000, 96_000]
        .into_iter()
        .map(|r| {
            let label = if r == crate::prefs::DEFAULT_SAMPLE_RATE {
                format!("{} kHz (default)", r as f32 / 1000.0)
            } else {
                format!("{} kHz", r as f32 / 1000.0)
            };
            Item::action(label, Cmd::SampleRate(r)).check(r == rate)
        })
        .collect();
    let buffers: Vec<Item> = [
        None,
        Some(64),
        Some(128),
        Some(256),
        Some(480),
        Some(512),
        Some(1024),
    ]
    .into_iter()
    .map(|b| {
        Item::action(
            b.map_or("Device Default".to_string(), |b| format!("{b} frames")),
            Cmd::BufferFrames(b),
        )
        .check(core.prefs.buffer_frames == b)
    })
    .collect();
    let mut midi_items = vec![
        Item::action("Enable MIDI Input", Cmd::MidiEnabled).check(!core.prefs.midi_disabled),
        Item::action("All Ports", Cmd::MidiAllPorts).check(core.prefs.midi_inputs.is_empty()),
        Item::Separator,
    ];
    for port in &core.midi_ports {
        midi_items.push(
            Item::action(port.clone(), Cmd::MidiPort(port.clone()))
                .check(core.prefs.midi_inputs.contains(port))
                .enabled(!core.prefs.midi_disabled),
        );
    }
    if core.midi_ports.is_empty() {
        midi_items.push(Item::action("(no MIDI ports found)", Cmd::RescanMidi).enabled(false));
    }
    midi_items.extend([
        Item::Separator,
        Item::action("Rescan Ports", Cmd::RescanMidi),
    ]);
    use bubble_project::{DuplicatePolicy as D, MouseMode as M, Scope as S};
    let audio = vec![
        Item::action("Outputs…", Cmd::Outputs),
        Item::action("Reassign Cart Outputs…", Cmd::ReassignOutputs),
        Item::sub("Sample Rate", rates),
        Item::sub("Buffer Size", buffers),
        Item::action("Output Safety Limiter", Cmd::Limiter).check(s.limiter),
        Item::sub("MIDI Inputs", midi_items),
        Item::Separator,
        Item::action("Stop All (fade)", Cmd::StopAll),
        Item::action("Stop Page (fade)", Cmd::StopPage),
        Item::action("Stop All Immediately", Cmd::StopAllNow).accel("CmdOrCtrl+Period"),
    ];
    let session = vec![
        Item::action("Session Settings…", Cmd::ProjectSettings).accel("CmdOrCtrl+Comma"),
        Item::action("Page Settings…", Cmd::PageSettings),
        Item::action("Add Page", Cmd::AddPage),
        Item::action("Remove Page", Cmd::RemovePage).enabled(snap.project.pages.len() > 1),
        Item::Separator,
        Item::sub(format!("Output Level ({level:.0} dB)"), levels),
        Item::sub(
            "Mouse",
            vec![
                Item::action("Click Toggles Play/Stop", Cmd::MouseToggle)
                    .check(s.mouse_mode == M::Toggle),
                Item::action("Left Plays, Right Stops", Cmd::MouseLeftRight)
                    .check(s.mouse_mode == M::LeftPlayRightStop),
            ],
        ),
        Item::sub(
            "Same Trigger on Several Carts",
            vec![
                Item::action("Ganged (all play)", Cmd::DuplicatesGanged)
                    .check(s.duplicate_binding_policy == D::Ganged),
                Item::action("Overlapped (next free one)", Cmd::DuplicatesOverlapped)
                    .check(s.duplicate_binding_policy == D::Overlapped),
            ],
        ),
        Item::sub(
            "Triggers Act On",
            vec![
                Item::action("All Pages", Cmd::ScopeGlobal).check(s.binding_scope == S::Global),
                Item::action("Current Page Only", Cmd::ScopePage)
                    .check(s.binding_scope == S::CurrentPage),
            ],
        ),
    ];
    let zoom = (core.prefs.ui_scale.unwrap_or(1.0) * 100.0).round() as u32;
    let view = vec![
        Item::sub(
            "Page Bar Size",
            ["Small", "Medium", "Large (touch screen)"]
                .into_iter()
                .enumerate()
                .map(|(i, n)| {
                    Item::action(n, Cmd::BarSize(i as u8)).check(core.prefs.bar_size == i as u8)
                })
                .collect(),
        ),
        Item::sub(
            "Interface Size",
            [75u32, 100, 125, 150, 200]
                .into_iter()
                .map(|z| Item::action(format!("{z}%"), Cmd::Zoom(z)).check(z == zoom))
                .collect(),
        ),
        Item::sub(
            "Appearance",
            [
                ("Follow System", Appearance::System),
                ("Light", Appearance::Light),
                ("Dark", Appearance::Dark),
            ]
            .into_iter()
            .map(|(n, a)| Item::action(n, Cmd::Appearance(a)).check(core.prefs.appearance == a))
            .collect(),
        ),
        Item::Separator,
        Item::action("Playout Log", Cmd::ShowLog).accel("CmdOrCtrl+L"),
        Item::action("Audio Diagnostics", Cmd::ShowDiagnostics),
        Item::action("Show Log File", Cmd::ShowLogs),
    ];
    let top = |title: &str, items| Top {
        title: title.into(),
        items,
    };
    Menu {
        tops: vec![
            top("File", file),
            top("Edit", edit),
            top("Session", session),
            top("Cart", cart),
            top("Audio", audio),
            top("View", view),
        ],
    }
}

/// Settings and preferences a menu command changes directly, for every interface:
/// returns true when it handled `cmd`. Session settings changes are pushed as an
/// [`crate::Intent::Settings`].
pub fn apply_common(
    core: &mut Core,
    cmd: &Cmd,
    snap: &Snapshot,
    intents: &mut Vec<crate::Intent>,
) -> bool {
    use bubble_project::{DuplicatePolicy as D, MouseMode as M, Scope as S};
    let mut settings = snap.project.settings.clone();
    match cmd {
        Cmd::SampleRate(r) => {
            core.prefs.sample_rate = *r;
            core.save_prefs();
            core.open_audio();
        }
        Cmd::BufferFrames(b) => {
            core.prefs.buffer_frames = *b;
            core.save_prefs();
            core.open_audio();
        }
        Cmd::MidiEnabled => {
            core.prefs.midi_disabled = !core.prefs.midi_disabled;
            core.reopen_midi();
        }
        Cmd::MidiAllPorts => {
            core.prefs.midi_inputs.clear();
            core.reopen_midi();
        }
        Cmd::MidiPort(name) => {
            if let Some(i) = core.prefs.midi_inputs.iter().position(|n| n == name) {
                core.prefs.midi_inputs.remove(i);
            } else {
                core.prefs.midi_inputs.push(name.clone());
            }
            core.reopen_midi();
        }
        Cmd::RescanMidi => {
            core.midi_ports = crate::midi::available_ports();
            core.reopen_midi();
        }
        Cmd::MouseToggle => settings.mouse_mode = M::Toggle,
        Cmd::MouseLeftRight => settings.mouse_mode = M::LeftPlayRightStop,
        Cmd::DuplicatesGanged => settings.duplicate_binding_policy = D::Ganged,
        Cmd::DuplicatesOverlapped => settings.duplicate_binding_policy = D::Overlapped,
        Cmd::ScopeGlobal => settings.binding_scope = S::Global,
        Cmd::ScopePage => settings.binding_scope = S::CurrentPage,
        Cmd::Limiter => settings.limiter = !settings.limiter,
        Cmd::OutputLevel(db) => settings.master_gain_db = *db as f32,
        Cmd::BarSize(b) => {
            core.prefs.bar_size = (*b).min(2);
            core.save_prefs();
        }
        _ => return false,
    }
    if settings != snap.project.settings {
        intents.push(crate::Intent::Settings(settings));
    }
    true
}
