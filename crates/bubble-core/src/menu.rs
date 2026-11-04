//! The application menus as data: every command, its label, shortcut and state.
//! Each interface shows this model its own way (native or in-window) and runs the
//! [`Cmd`]s chosen from it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use uuid::Uuid;

use bubble_control::AttrSet;

use crate::app::{Core, Snapshot};
use crate::prefs::Appearance;

/// Output level presets offered in the Session menu (dB).
pub const OUTPUT_LEVELS: [i32; 7] = [0, -3, -6, -10, -12, -15, -20];

/// What Edit ▸ Paste Attributes copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PasteAttr {
    GainMute,
    Pan,
    Fades,
    Colour,
    Loop,
    /// Retrigger, stop others, group and hold to play.
    Behaviour,
    All,
}

impl PasteAttr {
    pub fn set(self) -> AttrSet {
        let none = AttrSet {
            gain: false,
            pan: false,
            fades: false,
            colour: false,
            looping: false,
            behaviour: false,
        };
        match self {
            PasteAttr::GainMute => AttrSet { gain: true, ..none },
            PasteAttr::Pan => AttrSet { pan: true, ..none },
            PasteAttr::Fades => AttrSet {
                fades: true,
                ..none
            },
            PasteAttr::Colour => AttrSet {
                colour: true,
                ..none
            },
            PasteAttr::Loop => AttrSet {
                looping: true,
                ..none
            },
            PasteAttr::Behaviour => AttrSet {
                behaviour: true,
                ..none
            },
            PasteAttr::All => AttrSet::ALL,
        }
    }
}

/// Everything a menu item can ask the app to do.
#[derive(Clone, Debug, PartialEq, Hash)]
pub enum Cmd {
    New,
    Open,
    OpenRecent(usize),
    /// File ▸ Close Session: an empty cart wall and the start question.
    CloseSession,
    Save,
    SaveAs,
    Relink,
    ReloadAudio,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    /// Edit ▸ Paste Attributes ▸ …: from the copied cart to the selected one.
    PasteAttributes(PasteAttr),
    ClearCart,
    AudioSetup,
    PlaySelected,
    ToggleEditMode,
    AddPage,
    RemovePage,
    StopAll,
    StopPage,
    StopAllNow,
    /// Audio ▸ Outputs… window (the Settings window's Audio section, where there is one).
    Outputs,
    /// This machine's settings (Bubble ▸ Settings… on a Mac, File ▸ Settings… elsewhere).
    Settings,
    /// Session ▸ Grid Size: the current page's rows and columns.
    GridSize(u16, u16),
    ReassignOutputs,
    MidiEnabled,
    MidiAllPorts,
    MidiPort(String),
    RescanMidi,
    MouseToggle,
    MouseLeftRight,
    ScopeGlobal,
    Limiter,
    ShowLog,
    ShowDiagnostics,
    /// Shows Bubble's log file (start-up and audio events) in the file manager.
    ShowLogs,
    /// Global output attenuation preset in dB.
    OutputLevel(i32),
    ProjectSettings,
    RevealSession,
    /// Session ▸ Restore from Backup ▸ that version (see [`Core::backup_versions`]).
    RestoreBackup(usize),
    /// Settings: whether double-clicking a .bubble file opens Bubble.
    OpenBubbleFiles,
    CopyPage,
    PastePage,
    DuplicateCart,
    PageSettings,
    SpeedBar,
    /// Height of the bottom page/stop bar (0 small, 1 medium, 2 large).
    BarSize(u8),
    /// The page bar at the top of the window (true) or the bottom.
    BarTop(bool),
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

/// A label in this platform's style: as written (Title Case) on a Mac, sentence case
/// elsewhere ("Stop all now", "Reassign cart outputs…"). Acronyms (MIDI, CSV), single
/// letters (Output A) and names (Bubble) keep their capitals.
pub fn t(label: &str) -> String {
    if cfg!(target_os = "macos") {
        return label.to_string();
    }
    let keep = |w: &str| {
        let core: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
        core.len() == 1 && core.chars().all(|c| c.is_ascii_uppercase())
            || core.len() > 1 && core.chars().all(|c| !c.is_lowercase())
            || ["Bubble", "SpotOn", "Finder"].contains(&core.as_str())
    };
    label
        .split(' ')
        .enumerate()
        .map(|(i, w)| {
            if i == 0 || keep(w) {
                return w.to_string();
            }
            // The first letter, after any opening bracket, made small.
            let mut out = String::with_capacity(w.len());
            let mut done = false;
            for c in w.chars() {
                if !done && c.is_alphabetic() {
                    out.extend(c.to_lowercase());
                    done = true;
                } else {
                    out.push(c);
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The menus for the current state.
pub fn build(core: &Core, snap: &Snapshot, st: &MenuState) -> Menu {
    let has_sel = st.selected.is_some();
    let recent: Vec<Item> = core
        .prefs
        .recent_projects
        .iter()
        .enumerate()
        .map(|(i, p)| Item::action(p.display().to_string(), Cmd::OpenRecent(i)))
        .collect();
    let mut file = vec![
        Item::action(t("New Session…"), Cmd::New).accel("CmdOrCtrl+N"),
        Item::action(t("Open…"), Cmd::Open).accel("CmdOrCtrl+O"),
    ];
    if !recent.is_empty() {
        file.push(Item::sub(t("Open Recent"), recent));
    }
    file.extend([
        Item::Separator,
        Item::action(t("Close Session"), Cmd::CloseSession)
            .accel("CmdOrCtrl+W")
            .enabled(snap.path.is_some() || snap.project.carts().next().is_some()),
        Item::action(t("Save"), Cmd::Save).accel("CmdOrCtrl+S"),
        Item::action(t("Save Session As…"), Cmd::SaveAs).accel("CmdOrCtrl+Shift+S"),
        Item::Separator,
        Item::action(
            if cfg!(target_os = "macos") {
                "Show Session in Finder".to_string()
            } else {
                t("Reveal Session Folder")
            },
            Cmd::RevealSession,
        )
        .enabled(snap.path.is_some()),
        Item::action(t("Relink Missing Files…"), Cmd::Relink),
        Item::action(t("Reload All Audio"), Cmd::ReloadAudio),
    ]);
    #[cfg(not(target_os = "macos"))]
    file.extend([
        Item::Separator,
        Item::action(t("Quit"), Cmd::Quit).accel("CmdOrCtrl+Q"),
    ]);

    let edit = vec![
        Item::action(t("Undo"), Cmd::Undo)
            .accel("CmdOrCtrl+Z")
            .enabled(snap.can_undo),
        Item::action(t("Redo"), Cmd::Redo)
            .accel("CmdOrCtrl+Shift+Z")
            .enabled(snap.can_redo),
        Item::Separator,
        Item::action(t("Cut Cart"), Cmd::Cut)
            .accel("CmdOrCtrl+X")
            .enabled(has_sel),
        Item::action(t("Copy Cart"), Cmd::Copy)
            .accel("CmdOrCtrl+C")
            .enabled(has_sel),
        Item::action(t("Paste Cart"), Cmd::Paste)
            .accel("CmdOrCtrl+V")
            .enabled(core.clipboard.is_some() && st.paste_target.is_some()),
        {
            let can = core.clipboard.is_some() && has_sel;
            let a = |label: &str, what: PasteAttr| {
                Item::action(t(label), Cmd::PasteAttributes(what)).enabled(can)
            };
            Item::sub(
                "Paste Attributes",
                vec![
                    a("Gain and Mute", PasteAttr::GainMute),
                    a("Pan", PasteAttr::Pan),
                    a("Fades", PasteAttr::Fades),
                    a("Colour", PasteAttr::Colour),
                    a("Loop", PasteAttr::Loop),
                    a(
                        "Behaviour (Retrigger, Stop Others, Group, Hold)",
                        PasteAttr::Behaviour,
                    ),
                    Item::Separator,
                    a("All Attributes", PasteAttr::All).accel("CmdOrCtrl+Shift+V"),
                ],
            )
        },
        Item::action(t("Duplicate Cart"), Cmd::DuplicateCart)
            .accel("CmdOrCtrl+D")
            .enabled(has_sel),
        Item::action(t("Clear Cart"), Cmd::ClearCart).enabled(has_sel),
        Item::Separator,
        Item::action(t("Edit Mode"), Cmd::ToggleEditMode)
            .accel("CmdOrCtrl+E")
            .check(st.edit_mode),
    ];
    let machine = core.prefs.machine();
    let level = machine.output_level_db;
    let mut levels: Vec<Item> = OUTPUT_LEVELS
        .iter()
        .map(|db| {
            Item::action(format!("{db} dB"), Cmd::OutputLevel(*db))
                .check((level - *db as f32).abs() < 0.05)
        })
        .collect();
    let custom = !OUTPUT_LEVELS
        .iter()
        .any(|db| (level - *db as f32).abs() < 0.05);
    // Custom… opens where the level is set: the Settings window.
    let custom_cmd = Cmd::Settings;
    levels.extend([
        Item::Separator,
        Item::action(
            if custom {
                format!("Custom ({level:.1} dB)…")
            } else {
                "Custom…".to_string()
            },
            custom_cmd,
        )
        .check(custom),
    ]);

    let cart = vec![
        Item::action(t("Cart Setup…"), Cmd::AudioSetup)
            .accel("CmdOrCtrl+I")
            .enabled(has_sel),
        Item::action(t("Speed Bar…"), Cmd::SpeedBar)
            .accel("CmdOrCtrl+B")
            .enabled(has_sel),
        Item::action(t("Play"), Cmd::PlaySelected)
            .accel("CmdOrCtrl+P")
            .enabled(has_sel),
    ];

    let pages = snap.project.pages.len();
    let (rows, cols) = snap
        .project
        .pages
        .get(snap.page)
        .map_or((4, 6), |p| (p.rows, p.cols));
    let mut sizes: Vec<(u16, u16)> = GRID_SIZES.to_vec();
    if !sizes.contains(&(rows, cols)) {
        sizes.push((rows, cols));
        sizes.sort();
    }
    let mut grid: Vec<Item> = sizes
        .into_iter()
        .map(|(r, c)| {
            Item::action(grid_label(r, c), Cmd::GridSize(r, c))
                .check((r, c) == (rows, cols))
                .enabled(grid_fits(snap, snap.page, r, c))
        })
        .collect();
    grid.extend([
        Item::Separator,
        Item::action(t("Other…"), Cmd::PageSettings),
    ]);

    let audio = vec![
        Item::action(t("Stop All (fade)"), Cmd::StopAll),
        Item::action(t("Stop Page (fade)"), Cmd::StopPage),
        // ⌘. is the Mac's own "stop"; Windows has no such convention.
        Item::action(t("Stop All Now"), Cmd::StopAllNow).accel(if cfg!(target_os = "macos") {
            "CmdOrCtrl+Period"
        } else {
            "CmdOrCtrl+Shift+A"
        }),
        Item::Separator,
        Item::sub(format!("{} ({level:.0} dB)", t("Output Level")), levels),
        Item::Separator,
        Item::action(t("Outputs…"), Cmd::Outputs),
        Item::action(t("Reassign Cart Outputs…"), Cmd::ReassignOutputs),
    ];
    let today = jiff::Zoned::now().date();
    let mut backups: Vec<Item> = core
        .backup_versions
        .iter()
        .enumerate()
        .map(|(i, (_, time))| {
            Item::action(crate::backups::label(*time, today), Cmd::RestoreBackup(i))
        })
        .collect();
    if backups.is_empty() {
        backups.push(Item::action(t("No backups yet"), Cmd::RestoreBackup(0)).enabled(false));
    }
    let session = vec![
        Item::action(t("Session Settings…"), Cmd::ProjectSettings).accel("CmdOrCtrl+Comma"),
        Item::sub(t("Restore from Backup"), backups),
        Item::Separator,
        Item::action(t("Add Page"), Cmd::AddPage),
        Item::action(t("Copy Page"), Cmd::CopyPage),
        Item::action(t("Paste Page (Replaces This One)"), Cmd::PastePage)
            .enabled(core.page_clip.is_some()),
        Item::sub(t("Grid Size"), grid),
        Item::Separator,
        Item::action(t("Delete Page…"), Cmd::RemovePage).enabled(pages > 1),
    ];
    let view = vec![
        Item::sub(
            "Page Bar Size",
            BAR_SIZES
                .into_iter()
                .enumerate()
                .map(|(i, n)| {
                    Item::action(t(n), Cmd::BarSize(i as u8)).check(core.prefs.bar_size == i as u8)
                })
                .collect(),
        ),
        Item::sub(
            "Appearance",
            APPEARANCES
                .into_iter()
                .map(|(n, a)| {
                    Item::action(t(n), Cmd::Appearance(a)).check(core.prefs.appearance == a)
                })
                .collect(),
        ),
        Item::Separator,
        Item::action(t("Playout Log"), Cmd::ShowLog).accel("CmdOrCtrl+L"),
        Item::action(t("Audio Diagnostics"), Cmd::ShowDiagnostics),
        Item::action(t("Show Log File"), Cmd::ShowLogs),
    ];
    // Windows and Linux: Settings… in File (macOS has it in the Bubble menu).
    if !cfg!(target_os = "macos") {
        let at = file.len() - 2;
        file.splice(
            at..at,
            [Item::Separator, Item::action(t("Settings…"), Cmd::Settings)],
        );
    }
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

/// Grid sizes offered in Session ▸ Grid Size (rows, columns).
pub const GRID_SIZES: [(u16, u16); 8] = [
    (3, 4),
    (3, 5),
    (4, 4),
    (4, 6),
    (4, 8),
    (5, 8),
    (6, 8),
    (8, 8),
];

/// Whether every cart on `page` fits a grid of this size (a grid never shrinks past
/// its carts).
pub fn grid_fits(snap: &Snapshot, page: usize, rows: u16, cols: u16) -> bool {
    snap.project
        .pages
        .get(page)
        .is_some_and(|p| p.cells.iter().all(|c| c.row < rows && c.col < cols))
}

/// "4 rows, 6 columns".
pub fn grid_label(rows: u16, cols: u16) -> String {
    let n = |v: u16, one: &str, many: &str| format!("{v} {}", if v == 1 { one } else { many });
    format!(
        "{}, {}",
        n(rows, "row", "rows"),
        n(cols, "column", "columns")
    )
}

/// Buffer sizes offered (frames; `None` = the device's own).
pub const BUFFER_SIZES: [Option<u32>; 7] = [
    None,
    Some(64),
    Some(128),
    Some(256),
    Some(480),
    Some(512),
    Some(1024),
];

pub fn buffer_label(b: Option<u32>) -> String {
    b.map_or("Device Default".to_string(), |b| format!("{b} frames"))
}

/// Sample rates offered for every output device.
pub const SAMPLE_RATES: [u32; 2] = [44_100, 48_000];

/// Page bar heights, in `Prefs::bar_size` order.
pub const BAR_SIZES: [&str; 3] = ["Small", "Medium", "Large"];

pub const APPEARANCES: [(&str, Appearance); 3] = [
    ("Follow System", Appearance::System),
    ("Light", Appearance::Light),
    ("Dark", Appearance::Dark),
];

/// Settings and preferences a menu command changes directly, for every interface:
/// returns true when it handled `cmd`. Session settings changes are pushed as an
/// [`crate::Intent::Settings`].
pub fn apply_common(
    core: &mut Core,
    cmd: &Cmd,
    snap: &Snapshot,
    intents: &mut Vec<crate::Intent>,
) -> bool {
    use bubble_project::{MouseMode as M, Scope as S};
    let mut settings = snap.project.settings.clone();
    match cmd {
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
            // Listed again on the helper thread.
            core.reopen_midi();
        }
        Cmd::MouseToggle => core.edit_machine(|p| p.mouse_mode = Some(M::Toggle)),
        Cmd::MouseLeftRight => core.edit_machine(|p| p.mouse_mode = Some(M::LeftPlayRightStop)),
        Cmd::ScopeGlobal => settings.binding_scope = S::Global,
        Cmd::Limiter => {
            let on = !core.prefs.machine().limiter;
            core.edit_machine(|p| p.limiter = Some(on));
        }
        Cmd::OutputLevel(db) => core.edit_machine(|p| p.output_level_db = Some(*db as f32)),
        Cmd::GridSize(r, c) => intents.push(crate::Intent::ResizePage(snap.page, *r, *c)),
        Cmd::BarSize(b) => {
            core.prefs.bar_size = (*b).min(2);
            core.save_prefs();
        }
        Cmd::BarTop(top) => {
            core.prefs.bar_top = *top;
            core.save_prefs();
        }
        _ => return false,
    }
    if settings != snap.project.settings {
        intents.push(crate::Intent::Settings(settings));
    }
    true
}
