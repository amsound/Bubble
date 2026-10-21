//! The Bubble window: cart grid, page/stop bar, cart inspector, status bar, and
//! the secondary windows (project and page settings, audio setup, speed bars).
//!
//! The UI renders from a per-frame snapshot (project clone + runtime map) and
//! sends changes back as [`Intent`]s. The controller lock is held only to take
//! the snapshot and to apply intents, so MIDI callbacks are never blocked by
//! rendering (decision D-06).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bubble_control::{
    AssetState, AttrSet, CartClip, CartRuntime, CartState, Controller, InputEvent, LogKind,
    MouseButton, TriggerSource, describe, display_label,
};
use bubble_engine::Overview;
use bubble_engine::device::{AudioOutput, OutputDeviceInfo, list_output_devices};
use bubble_project::{
    Action, Cart, Curve, ExclusiveGroup, FadeSpec, HAlign, Project, ProjectSettings, Retrigger,
    StopScope, VAlign,
};
use eframe::egui::{self, Color32, Key, PointerButton, Pos2, Rect, Sense, Vec2};
use uuid::Uuid;

mod audio_out;
mod convert;
mod crash;
mod outputs_ui;
mod session_ui;
mod setup_ui;

use crate::dialogs::{AUDIO_EXTS, FileDialogs, OPEN_EXTS, Purpose};
use crate::fonts::FontState;
use crate::menu::{Cmd, Item, Menu, Top};
use crate::meter::{Ppm, ppm_ui};
use crate::midi::{MidiInputs, available_ports};
use crate::native::{WindowSpec, native_window};
use crate::prefs::Prefs;
use crate::tile::{DEFAULT_CART_COLOR, TileState, fmt_precise, parse_color};
use crate::waveform::{TrimEdit, WaveView, waveform_ui};
use session_ui::{AfterCreate, Job, NewSession};

/// Output level presets offered in the Audio menu (dB).
const OUTPUT_LEVELS: [i32; 7] = [0, -3, -6, -10, -12, -15, -20];
/// Quick colour choices for tiles.
const SWATCHES: [[u8; 3]; 10] = [
    [0x3A, 0x6E, 0xA5],
    [0x2E, 0x7D, 0x32],
    [0xC6, 0x28, 0x28],
    [0xEF, 0x6C, 0x00],
    [0xF9, 0xA8, 0x25],
    [0x6A, 0x1B, 0x9A],
    [0xAD, 0x14, 0x57],
    [0x00, 0x83, 0x8F],
    [0x5D, 0x40, 0x37],
    [0x54, 0x5B, 0x64],
];
/// A project being read in the background: (path, opened at start-up?, result).
type PendingOpen = (
    PathBuf,
    bool,
    std::sync::mpsc::Receiver<Result<Project, String>>,
);

const AUTOSAVE_EVERY: Duration = Duration::from_secs(30);

/// Changes requested by the UI during a frame, applied after rendering.
#[allow(clippy::large_enum_variant)] // short-lived, a few per frame
enum Intent {
    PointerDown(Uuid, MouseButton),
    PointerUp(Uuid),
    StopAll(bool),
    StopPage(usize),
    SetPage(usize),
    AssignFiles(usize, u16, u16, Vec<PathBuf>),
    ClearCell(usize, u16, u16),
    MoveCell((usize, u16, u16), (usize, u16, u16)),
    ReplaceCart(Cart),
    SetGroups(Vec<ExclusiveGroup>),
    AddGroup,
    SetBindingAction(Uuid, Action),
    LiveSpeed(Uuid, f32),
    /// Speed bar x1: ramp back to normal speed over the cart's speed-up time.
    SpeedNormal(Uuid),
    SlowToStop(Uuid),
    ReleaseHolds,
    /// Learn a trigger with the default action for the current mouse mode.
    LearnDefault(Uuid),
    Settings(ProjectSettings),
    RenameProject(String),
    AddPage,
    MovePage(usize, usize),
    ReassignOutputs(Option<String>, String, bubble_control::ReassignScope),
    /// Add an imported project's pages to this session.
    Append(Project),
    CreateControl(usize, u16, u16, bubble_project::Control, Option<Uuid>),
    PastePage(usize, bubble_control::PageClip),
    RemovePage(usize),
    RenamePage(usize, String),
    ResizePage(usize, u16, u16),
    CancelLearn,
    ClearBindings(Uuid, Option<bool>),
    Relink(Uuid, PathBuf),
    Refresh,
    Input(InputEvent),
    Open(PathBuf),
    Save,
    /// Start a new, empty session saved at this file (its folder already exists).
    CreateSession(PathBuf, String),
    SaveAsWith(PathBuf, Vec<(Uuid, PathBuf)>),
    Retarget(Vec<(Uuid, PathBuf)>),
    /// Assets converted to WAV: repoint them and rescale their trims.
    Converted(Vec<bubble_control::Conversion>),
    Trigger(Uuid),
    /// Play a cart with unsaved Cart Setup settings.
    LiveGain(Uuid, f32, bool),
    LiveSpeedRamp(Uuid, f32, u32),
    /// Cart Setup: turn looping on or off on the playing preview.
    LiveLoop(Uuid, bool),
    /// Cart Setup: new In and Out (seconds) for the playing preview.
    LiveTrim(Uuid, f64, f64),
    /// Cart Setup: move the playing preview to seconds from the file start.
    Seek(Uuid, f64),
    /// Cart Setup: start the preview at seconds from the file start.
    PreviewAt(Cart, f64),
    /// Stop a cart at once, ignoring its fade-out.
    KillCart(Uuid),
    Preview(Cart),
    Undo,
    Redo,
    Paste(usize, u16, u16, CartClip),
    PasteAttrs(Cart, Vec<Uuid>, AttrSet),
}

struct Snapshot {
    project: Project,
    runtime: HashMap<Uuid, CartRuntime>,
    page: usize,
    dirty: bool,
    path: Option<PathBuf>,
    learning: Option<bubble_control::Learn>,
    pending: usize,
    missing: Vec<Uuid>,
    asset_info: HashMap<Uuid, String>,
    can_undo: bool,
    can_redo: bool,
    /// Output peaks per playing cart since the last frame (linear).
    peaks: HashMap<Uuid, (f32, f32)>,
}

#[derive(Default)]
struct Dialogs {
    diagnostics: bool,
    /// Cart whose Audio Setup (waveform/trim) window is open.
    audio_setup: Option<Uuid>,
    paste_attrs: bool,
    /// Outputs window draft (open while `Some`).
    outputs: Option<outputs_ui::OutputsDraft>,
    reassign: Option<outputs_ui::ReassignDraft>,
    project_settings: bool,
    page_settings: Option<usize>,
    /// Carts with an open speed bar window.
    speed_bars: Vec<Uuid>,
    /// Autosaves found at start-up: (autosave file, project it belongs to).
    recover: Vec<(PathBuf, Option<PathBuf>)>,
    relink: bool,
    log: bool,
    confirm_close: bool,
    confirm_discard: Option<PendingAction>,
    new_session: Option<NewSession>,
    /// A SpotOn file to import: add to the open session, or start a new one?
    import_choice: Option<PathBuf>,
    /// Files to offer converting to WAV (session setting).
    convert_offer: Option<Vec<(Uuid, PathBuf)>>,
    error: Option<String>,
}

#[derive(Clone)]
enum PendingAction {
    New,
    Open(PathBuf),
    /// Import a SpotOn session or package into a new session.
    Import(PathBuf),
}

pub struct BubbleApp {
    ctrl: Arc<Mutex<Controller>>,
    /// One running stream per output device (output A's device first).
    audio: Vec<AudioOutput>,
    /// The device each stream was asked for ("" = system default), parallel to `audio`.
    audio_keys: Vec<String>,
    /// Devices that are missing, and the check for their return.
    watch: audio_out::DeviceWatch,
    audio_error: Option<String>,
    /// Outputs that could not be opened ("Output B: …").
    output_errors: Vec<String>,
    midi: MidiInputs,
    prefs: Prefs,
    edit_mode: bool,
    selected: Option<Uuid>,
    pressed: Option<Uuid>,
    dragging: Option<(usize, u16, u16)>,
    /// The drag duplicates (⌥ Option) instead of moving (Shift).
    drag_copy: bool,
    /// Keyboard cursor on an empty cell (arrow keys); a cart cell uses `selected`.
    cursor_empty: Option<(usize, u16, u16)>,
    dialogs: Dialogs,
    last_autosave: Instant,
    allow_close: bool,
    repaint: Arc<dyn Fn() + Send + Sync>,
    files: FileDialogs,
    clipboard: Option<CartClip>,
    /// Project being read in the background: (path, startup?, result channel).
    opening: Option<PendingOpen>,
    /// Identifies this run's untitled recovery file.
    session_id: u64,
    started: Instant,
    /// The main window moved or resized; saved to preferences once it settles.
    window_moved: Option<Instant>,
    /// A session or SpotOn file named on the command line, opened on the first frame.
    open_at_start: Option<PathBuf>,
    /// When the open session started loading its audio (logged once it is ready).
    loading_since: Option<Instant>,
    /// Most audio files seen waiting at once, for the "n of total" progress.
    loading_total: usize,
    first_frame_logged: bool,
    /// Set when the user chose to quit without saving.
    discard_on_exit: bool,
    attr_choice: AttrSet,
    wave_view: WaveView,
    devices: Vec<OutputDeviceInfo>,
    midi_ports: Vec<String>,
    fonts: FontState,
    font_names: Vec<&'static str>,
    meters: HashMap<Uuid, Ppm>,
    last_frame: Instant,
    /// Cart whose quick-edit popup is open, and where.
    quick_edit: Option<(Uuid, Pos2)>,
    /// Page tab being dragged to a new position.
    page_drag: Option<usize>,
    /// Page tab being renamed in place, and the text so far.
    renaming_page: Option<(usize, String)>,
    /// Copied page (Copy Page / Paste Page).
    page_clip: Option<bubble_control::PageClip>,
    /// Cart Setup's unapplied edits, and whether closing asked to keep them.
    setup_draft: Option<Cart>,
    setup_close_prompt: bool,
    /// Cart Setup's preview is paused (held at speed 0).
    setup_paused: bool,
    /// Bring the Cart Setup window to the front on the next frame.
    focus_setup: bool,
    /// Inline colour pickers open in quick edit / Cart Setup.
    qe_picker: bool,
    setup_picker: bool,
    /// Pointer position during an OS file drag (window points).
    drag_hover: Option<Pos2>,
    /// Background copy/import in progress.
    job: Option<Job>,
    /// Collect outside audio into the session once nothing else is running.
    want_collect: bool,
    /// A WAV conversion check is due (`true`: offer it first).
    wav_check: Option<bool>,
    /// When the session file was last written (for the title).
    saved_at: Option<std::time::SystemTime>,
    last_title: String,
    /// Speed bars showing the ramp-time choices: true = x0 slow-down, false = x1.
    speed_pick: HashMap<Uuid, bool>,
    /// Whether any Bubble window had keyboard focus last frame.
    had_focus: bool,
    /// Set by secondary windows during the frame.
    child_focus: bool,
    #[cfg(target_os = "macos")]
    native_menu: crate::menu::NativeMenu,
}

impl BubbleApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let prefs = Prefs::load();
        if let Some(scale) = prefs.ui_scale {
            cc.egui_ctx.set_zoom_factor(scale);
        }
        cc.egui_ctx.set_theme(prefs.appearance);
        // The last project is opened in the background (see `start_open`): reading a
        // file in a protected folder can block on a macOS privacy prompt, and the
        // window must appear regardless.
        let ctrl = Controller::new(Controller::new_project("Untitled"), None);
        let startup_error: Option<String> = None;
        let ctrl = Arc::new(Mutex::new(ctrl));
        let ctx = cc.egui_ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let mut app = BubbleApp {
            ctrl,
            audio: Vec::new(),
            audio_keys: Vec::new(),
            watch: Default::default(),
            audio_error: None,
            output_errors: Vec::new(),
            midi: MidiInputs::empty(),
            prefs,
            edit_mode: true,
            selected: None,
            pressed: None,
            dragging: None,
            drag_copy: false,
            cursor_empty: None,
            dialogs: Dialogs {
                error: startup_error,
                ..Default::default()
            },
            last_autosave: Instant::now(),
            allow_close: false,
            files: FileDialogs::new(repaint.clone()),
            clipboard: None,
            opening: None,
            started: Instant::now(),
            window_moved: None,
            open_at_start: None,
            loading_since: None,
            loading_total: 0,
            first_frame_logged: false,
            session_id: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            discard_on_exit: false,
            attr_choice: AttrSet::ALL,
            wave_view: WaveView::default(),
            devices: list_output_devices().unwrap_or_default(),
            midi_ports: available_ports(),
            fonts: FontState::default(),
            font_names: crate::fonts::available(),
            meters: HashMap::new(),
            last_frame: Instant::now(),
            quick_edit: None,
            speed_pick: HashMap::new(),
            job: None,
            drag_hover: None,
            qe_picker: false,
            setup_draft: None,
            setup_close_prompt: false,
            setup_paused: false,
            focus_setup: false,
            setup_picker: false,
            page_drag: None,
            renaming_page: None,
            page_clip: None,
            want_collect: false,
            wav_check: None,
            saved_at: None,
            last_title: String::new(),
            had_focus: true,
            child_focus: false,
            #[cfg(target_os = "macos")]
            native_menu: crate::menu::NativeMenu::new(),
            repaint,
        };
        #[cfg(target_os = "macos")]
        {
            let r = app.repaint.clone();
            crate::menu::NativeMenu::set_wakeup(move || r());
        }
        crash::install(app.ctrl.clone(), app.session_id);
        app.open_audio();
        for a in &app.audio {
            crate::log::info(&format!(
                "audio output: {} at {} Hz, {} ch, {}",
                a.device_name, a.sample_rate, a.channels, a.sample_format
            ));
        }
        for e in app.output_errors.iter().chain(app.audio_error.iter()) {
            crate::log::info(&format!("audio problem: {e}"));
        }
        app.open_midi();
        app.find_recoverable();
        if let Some(report) = crash::take_unreported() {
            app.dialogs.error = Some(format!(
                "Bubble quit unexpectedly last time.\n\nA crash report was saved to:\n{}\n\n\
                 Any unsaved work it could keep is offered for recovery.",
                report.display()
            ));
        }
        // A file given at launch (dropped on Bubble.exe, Open With → Bubble) opens
        // instead of the last session.
        app.open_at_start = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .find(|p| p.is_file());
        if app.open_at_start.is_none()
            && let Some(p) = app.prefs.last_project.clone()
        {
            app.start_open(p, true);
        }
        app
    }

    /// A strip across the top while Bubble is busy (opening, importing, copying,
    /// converting, preparing audio), so long operations are never mistaken for a hang.
    fn progress_banner(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
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
        if task.is_none() && loading.is_none() {
            return;
        }
        let accent = Color32::from_rgb(0x2F, 0x6B, 0xFF);
        egui::Panel::top("progress")
            .frame(egui::Frame::new().fill(accent).inner_margin(egui::Margin::symmetric(12, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(Color32::WHITE));
                    let text = match (&task, loading) {
                        (Some(t), _) => t.clone(),
                        (None, Some((done, total))) => {
                            format!("Preparing audio: {done} of {total} files ready")
                        }
                        (None, None) => String::new(),
                    };
                    ui.label(egui::RichText::new(text).strong().color(Color32::WHITE));
                    if let Some((done, total)) = loading {
                        ui.add(
                            egui::ProgressBar::new(done as f32 / total as f32)
                                .desired_width(ui.available_width().min(320.0))
                                .fill(Color32::WHITE),
                        );
                    }
                });
            });
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    /// Keeps the main window's place in preferences (saved a second after it stops
    /// changing, and on exit), so the next launch opens it in the same place.
    fn track_window(&mut self, ctx: &egui::Context) {
        let (outer, inner, maximized, fullscreen, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.maximized,
                v.fullscreen,
                v.minimized,
            )
        });
        if minimized == Some(true) || fullscreen == Some(true) {
            return;
        }
        let (Some(outer), Some(inner)) = (outer, inner) else {
            return;
        };
        // Viewport rects are in UI points, which include the interface zoom; the
        // window is placed in logical points.
        let z = ctx.zoom_factor();
        let maximized = maximized == Some(true);
        let place = match (maximized, self.prefs.window) {
            // Keep the un-maximised place to return to.
            (true, Some(w)) => crate::prefs::WindowPlace {
                maximized: true,
                ..w
            },
            _ => crate::prefs::WindowPlace {
                x: outer.min.x * z,
                y: outer.min.y * z,
                w: inner.width() * z,
                h: inner.height() * z,
                maximized,
            },
        };
        if self.prefs.window != Some(place) {
            self.prefs.window = Some(place);
            self.window_moved = Some(Instant::now());
        }
        if self
            .window_moved
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.window_moved = None;
            let _ = self.prefs.save();
        } else if self.window_moved.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn lock(&self) -> MutexGuard<'_, Controller> {
        self.ctrl.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// "Output A", "Output A - PA", or with `detail` the device and channels.
    /// Labels for every output a cart can pick (plus `extra`'s, if not set up here).
    fn output_labels(&self, extra: &str) -> Vec<String> {
        let n = self
            .prefs
            .effective_outputs()
            .len()
            .max(bubble_project::output_index(extra).map_or(0, |i| i + 1));
        (0..n).map(|i| self.output_label(i, false)).collect()
    }

    fn output_label(&self, index: usize, detail: bool) -> String {
        let outs = self.prefs.effective_outputs();
        let letter = bubble_project::output_letter(index);
        let Some(o) = outs.get(index) else {
            return format!("Output {letter} (not set up here)");
        };
        let device = if o.device.is_empty() {
            "System default"
        } else {
            &o.device
        };
        match (o.alias.trim().is_empty(), detail) {
            (false, false) => format!("Output {letter} - {}", o.alias.trim()),
            (false, true) => format!(
                "Output {letter} - {} ({device} {})",
                o.alias.trim(),
                o.channel_text()
            ),
            (true, _) => format!("Output {letter} - {device} {}", o.channel_text()),
        }
    }

    fn open_midi(&mut self) {
        self.midi = if self.prefs.midi_disabled {
            MidiInputs::empty()
        } else {
            let repaint = self.repaint.clone();
            MidiInputs::open(&self.ctrl, &self.prefs.midi_inputs, move || repaint())
        };
    }

    fn snapshot(&self) -> Snapshot {
        let mut c = self.lock();
        c.tick();
        let project = c.project().clone();
        let mut asset_info = HashMap::new();
        for a in &project.assets {
            let s = match c.asset_state(a.id) {
                Some(AssetState::Ready {
                    path,
                    audio,
                    source,
                    changed,
                }) => format!(
                    "{}\n{} Hz, {} ch, {:.2} s{}",
                    path.display(),
                    source.sample_rate,
                    source.channels,
                    audio.duration_secs(),
                    if *changed {
                        "\n⚠ file changed since it was assigned"
                    } else {
                        ""
                    }
                ),
                Some(AssetState::Loading { path }) => format!("{}\nloading…", path.display()),
                Some(AssetState::Missing { tried }) => format!(
                    "MISSING: {}\ntried:\n{}",
                    a.path,
                    tried
                        .iter()
                        .map(|p| format!("  {}", p.display()))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
                Some(AssetState::Error { path, message }) => {
                    format!("{}\nERROR: {message}", path.display())
                }
                None => a.path.clone(),
            };
            asset_info.insert(a.id, s);
        }
        Snapshot {
            can_undo: c.can_undo(),
            can_redo: c.can_redo(),
            peaks: c.take_cart_peaks(),
            runtime: c.runtime(),
            page: c.current_page(),
            dirty: c.is_dirty(),
            path: c.path().map(|p| p.to_path_buf()),
            learning: c.learning(),
            pending: c.assets_pending(),
            missing: c.missing_assets(),
            asset_info,
            project,
        }
    }

    fn apply(&mut self, intents: Vec<Intent>) {
        let mut reopen_prefs = false;
        let ctrl = self.ctrl.clone();
        for intent in intents {
            let mut c = ctrl.lock().unwrap_or_else(|p| p.into_inner());
            let result: Result<(), String> = match intent {
                Intent::PointerDown(id, b) => {
                    c.pointer_down(id, b);
                    Ok(())
                }
                Intent::PointerUp(id) => {
                    c.pointer_up(id);
                    Ok(())
                }
                Intent::StopAll(fade) => {
                    c.stop_all(fade);
                    Ok(())
                }
                Intent::StopPage(p) => {
                    c.stop_page(p);
                    Ok(())
                }
                Intent::SetPage(p) => {
                    c.set_current_page(p);
                    Ok(())
                }
                Intent::AssignFiles(p, r, col, files) => {
                    let errors: Vec<String> = c
                        .assign_files(p, r, col, &files)
                        .into_iter()
                        .filter_map(|r| r.err())
                        .collect();
                    if let Some(first) = c
                        .project()
                        .pages
                        .get(p)
                        .and_then(|pg| pg.cell(r, col))
                        .map(|x| x.cart.id)
                    {
                        self.selected = Some(first);
                    }
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(errors.join("\n"))
                    }
                }
                Intent::ClearCell(p, r, col) => {
                    c.clear_cell(p, r, col);
                    Ok(())
                }
                Intent::MoveCell(a, b) => {
                    c.move_cell(a, b);
                    Ok(())
                }
                Intent::ReplaceCart(cart) => {
                    let id = cart.id;
                    c.edit_cart(id, |x| *x = cart);
                    Ok(())
                }
                Intent::SetGroups(g) => {
                    c.set_groups(g);
                    Ok(())
                }
                Intent::AddGroup => {
                    c.add_group();
                    Ok(())
                }
                Intent::SetBindingAction(b, a) => {
                    c.set_binding_action(b, a);
                    Ok(())
                }
                Intent::LiveSpeed(id, v) => {
                    c.set_live_speed(id, v, 40);
                    Ok(())
                }
                Intent::SpeedNormal(id) => {
                    c.speed_to_normal(id);
                    Ok(())
                }
                Intent::SlowToStop(id) => {
                    c.slow_to_stop(id);
                    Ok(())
                }
                Intent::ReleaseHolds => {
                    c.release_holds();
                    Ok(())
                }
                Intent::LearnDefault(id) => {
                    let a = c.default_trigger_action();
                    c.start_learn(id, a);
                    Ok(())
                }
                Intent::Settings(s) => {
                    let wav_on = s.convert_to_wav && !c.project().settings.convert_to_wav;
                    c.edit_settings(|x| *x = s);
                    if wav_on {
                        self.want_wav_check(true);
                    }
                    Ok(())
                }
                Intent::RenameProject(n) => {
                    c.rename_project(&n);
                    Ok(())
                }
                Intent::AddPage => {
                    let n = c.project().pages.len() + 1;
                    let (r, col) = c
                        .project()
                        .pages
                        .last()
                        .map_or((4, 6), |p| (p.rows, p.cols));
                    let p = c.add_page(&format!("Page {n}"), r, col);
                    c.set_current_page(p);
                    Ok(())
                }
                Intent::RemovePage(p) => c.remove_page(p).map_err(|e| e.to_string()),
                Intent::CreateControl(p, r, col, kind, group) => {
                    if let Some(id) = c.create_control(p, r, col, kind, group) {
                        self.selected = Some(id);
                    }
                    Ok(())
                }
                Intent::ReassignOutputs(from, to, scope) => {
                    let n = c.reassign_outputs(from.as_deref(), &to, &scope);
                    if n == 0 {
                        Err("No carts matched.".to_string())
                    } else {
                        Ok(())
                    }
                }
                Intent::Append(p) => {
                    let first = c.append_project(p);
                    c.set_current_page(first);
                    Ok(())
                }
                Intent::MovePage(a, b) => {
                    c.move_page(a, b);
                    Ok(())
                }
                Intent::PastePage(p, clip) => {
                    c.paste_page(p, &clip);
                    Ok(())
                }
                Intent::RenamePage(p, n) => {
                    c.rename_page(p, &n);
                    Ok(())
                }
                Intent::ResizePage(p, r, col) => {
                    c.resize_page(p, r, col).map_err(|e| e.to_string())
                }
                Intent::CancelLearn => {
                    c.cancel_learn();
                    Ok(())
                }
                Intent::ClearBindings(id, k) => {
                    c.clear_bindings(id, k);
                    Ok(())
                }
                Intent::Relink(a, p) => {
                    let r = c.relink(a, &p).map(|_| ()).map_err(|e| e.to_string());
                    self.want_collect = true;
                    self.want_wav_check(false);
                    r
                }
                Intent::Retarget(moves) => {
                    c.retarget_assets(&moves);
                    Ok(())
                }
                Intent::Converted(done) => {
                    c.apply_conversions(&done);
                    Ok(())
                }
                Intent::CreateSession(file, name) => {
                    c.replace_project(Controller::new_project(&name), None);
                    self.selected = None;
                    match c.save_as(&file) {
                        Ok(()) => {
                            self.saved_at = Some(std::time::SystemTime::now());
                            self.prefs.add_recent(file);
                            reopen_prefs = true;
                            Ok(())
                        }
                        Err(e) => Err(e.to_string()),
                    }
                }
                Intent::SaveAsWith(p, moves) => match c.save_as_with(&p, &moves) {
                    Ok(()) => {
                        self.saved_at = Some(std::time::SystemTime::now());
                        if let Some(d) = recovery_dir() {
                            let _ = std::fs::remove_file(
                                d.join(format!("Untitled-{}.bubble", self.session_id)),
                            );
                        }
                        self.prefs.add_recent(p);
                        reopen_prefs = true;
                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                },
                Intent::Refresh => {
                    c.refresh_assets();
                    Ok(())
                }
                Intent::Input(ev) => {
                    c.handle_input(&ev);
                    Ok(())
                }
                Intent::Open(p) => {
                    drop(c);
                    self.start_open(p, false);
                    continue;
                }
                Intent::LiveGain(id, db, muted) => {
                    c.set_live_gain(id, db, muted);
                    Ok(())
                }
                Intent::LiveSpeedRamp(id, v, ms) => {
                    c.set_live_speed(id, v, ms);
                    Ok(())
                }
                Intent::LiveLoop(id, looping) => {
                    c.set_live_loop(id, looping);
                    Ok(())
                }
                Intent::LiveTrim(id, a, b) => {
                    c.set_live_trim(id, a, b);
                    Ok(())
                }
                Intent::Seek(id, s) => {
                    c.seek(id, s);
                    Ok(())
                }
                Intent::PreviewAt(cart, s) => {
                    c.preview_at(&cart, s);
                    Ok(())
                }
                Intent::KillCart(id) => {
                    c.stop_cart(id, false);
                    Ok(())
                }
                Intent::Preview(cart) => {
                    c.preview(&cart);
                    Ok(())
                }
                Intent::Trigger(id) => {
                    c.trigger(id, TriggerSource::Ui, None);
                    Ok(())
                }
                Intent::Undo => {
                    c.undo();
                    Ok(())
                }
                Intent::Redo => {
                    c.redo();
                    Ok(())
                }
                Intent::Paste(p, r, col, clip) => {
                    if let Some(id) = c.paste_cart(p, r, col, &clip) {
                        self.selected = Some(id);
                    }
                    Ok(())
                }
                Intent::PasteAttrs(from, targets, what) => {
                    c.paste_attributes(&from, &targets, what);
                    Ok(())
                }
                Intent::Save => match c.save() {
                    Ok(()) => {
                        drop(c);
                        self.saved_at = Some(std::time::SystemTime::now());
                        self.remove_autosave();
                        continue;
                    }
                    Err(e) => Err(e.to_string()),
                },
            };
            drop(c);
            if let Err(e) = result {
                self.dialogs.error = Some(e);
            }
        }
        if reopen_prefs {
            let _ = self.prefs.save();
        }
    }

    /// Reads a project on a helper thread; `poll_open` applies it.
    fn start_open(&mut self, path: PathBuf, startup: bool) {
        crate::log::info(&format!("opening {}", path.display()));
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = self.repaint.clone();
        let p = path.clone();
        std::thread::Builder::new()
            .name("bubble-open".into())
            .spawn(move || {
                let r = bubble_project::io::load(&p).map_err(|e| e.to_string());
                let _ = tx.send(r);
                repaint();
            })
            .expect("spawn open thread");
        self.opening = Some((path, startup, rx));
    }

    fn poll_open(&mut self) {
        let Some((path, startup, rx)) = &self.opening else {
            return;
        };
        let Ok(result) = rx.try_recv() else { return };
        let (path, startup) = (path.clone(), *startup);
        self.opening = None;
        match result {
            Ok(project) => {
                crate::log::info(&format!(
                    "opened: {} page(s), {} audio file(s)",
                    project.pages.len(),
                    project.assets.len()
                ));
                self.loading_since = Some(Instant::now());
                self.lock().replace_project(project, Some(path.clone()));
                self.selected = None;
                self.saved_at = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                self.want_collect = true;
                self.want_wav_check(true);
                self.prefs.add_recent(path);
                let _ = self.prefs.save();
                self.find_recoverable();
            }
            // A last project that no longer exists is not worth an error at start-up.
            Err(_) if startup && !path.exists() => {}
            Err(e) => {
                crate::log::info(&format!("could not open {}: {e}", path.display()));
                self.dialogs.error = Some(format!("Could not open {}:\n{e}", path.display()));
            }
        }
    }

    fn autosave(&mut self) {
        if self.last_autosave.elapsed() < AUTOSAVE_EVERY {
            return;
        }
        self.last_autosave = Instant::now();
        self.write_autosave();
    }

    /// Writes unsaved changes next to the project (`<file>.autosave`) or, for an
    /// untitled project, into the recovery folder. Never overwrites the project file.
    fn write_autosave(&mut self) {
        let c = self.lock();
        if !c.is_dirty() {
            return;
        }
        let json = bubble_project::io::to_json_string(c.project());
        let target = match c.path() {
            Some(p) => Some(autosave_path(p)),
            None => recovery_dir().map(|d| d.join(format!("Untitled-{}.bubble", self.session_id))),
        };
        drop(c);
        if let Some(t) = target {
            let _ = bubble_project::io::write_atomic(&t, json.as_bytes(), false);
        }
    }

    fn remove_autosave(&self) {
        let c = self.lock();
        if let Some(p) = c.path() {
            let _ = std::fs::remove_file(autosave_path(p));
        }
        if let Some(d) = recovery_dir() {
            let _ = std::fs::remove_file(d.join(format!("Untitled-{}.bubble", self.session_id)));
        }
    }

    /// Looks for unsaved work left by a previous session.
    fn find_recoverable(&mut self) {
        let mut found = Vec::new();
        if let Some(p) = self.lock().path().map(|p| p.to_path_buf()) {
            let a = autosave_path(&p);
            let newer = match (std::fs::metadata(&a), std::fs::metadata(&p)) {
                (Ok(am), Ok(pm)) => am.modified().ok() > pm.modified().ok(),
                (Ok(_), Err(_)) => true,
                _ => false,
            };
            if newer {
                found.push((a, Some(p)));
            }
        }
        if let Some(d) = recovery_dir()
            && let Ok(rd) = std::fs::read_dir(d)
        {
            {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension()
                        .is_some_and(|x| x == bubble_project::io::EXTENSION)
                    {
                        found.push((p, None));
                    }
                }
            }
        }
        self.dialogs.recover = found;
    }

    fn save_or_save_as(&mut self, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if snap.path.is_some() {
            intents.push(Intent::Save);
        } else {
            let name = if snap.project.name == "Untitled" {
                String::new()
            } else {
                snap.project.name.clone()
            };
            self.begin_new_session(name, AfterCreate::SaveAs);
        }
    }

    /// Turns finished file dialogs into intents.
    fn file_results(&mut self, snap: &Snapshot, intents: &mut Vec<Intent>) {
        for (purpose, paths) in self.files.poll() {
            match purpose {
                Purpose::Assign { page, row, col, .. } => {
                    self.assign_into_session(page, row, col, paths)
                }
                Purpose::Relink(a) => intents.push(Intent::Relink(a, paths[0].clone())),
                Purpose::Open => self.open_any(paths[0].clone(), snap, intents),
                Purpose::ChooseFolder => self.set_session_location(paths[0].clone()),
            }
        }
    }
}

fn autosave_path(project: &std::path::Path) -> PathBuf {
    let mut s = project.as_os_str().to_owned();
    s.push(".autosave");
    PathBuf::from(s)
}

fn recovery_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble")
        .map(|d| d.data_dir().join("Recovered"))
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Maps an egui key to the project's physical key-code names.
/// Shows a file in the platform's file manager.
fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Toggle => "Play / stop",
        Action::Play => "Play (restart)",
        Action::Stop => "Stop",
        _ => "Other",
    }
}

/// Left-aligned label in a fixed-width column, so form rows line up.
fn field_label(ui: &mut egui::Ui, text: &str) {
    field_label_w(ui, text, 104.0);
}

fn field_label_w(ui: &mut egui::Ui, text: &str, w: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(w, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(w);
            ui.label(egui::RichText::new(text).weak());
        },
    );
}

/// "100", "100%" or "1.5×" → 1.0 / 1.0 / 1.5 (sliders shown as percentages).
fn parse_percent(s: &str) -> Option<f64> {
    let t = s.trim();
    if let Some(x) = t.strip_suffix('×').or(t.strip_suffix('x')) {
        return x.trim().parse().ok();
    }
    t.trim_end_matches('%')
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v / 100.0)
}

#[derive(Clone, Copy)]
enum Icon {
    ToIn,
    Play,
    Pause,
    Stop,
}

fn icon_button_sized(ui: &mut egui::Ui, icon: Icon, size: Vec2) -> egui::Response {
    let resp = ui.add_sized(size, egui::Button::new(""));
    let c = resp.rect.center();
    let col = ui.visuals().strong_text_color();
    let p = ui.painter();
    let s = 7.0;
    let tri = |p: &egui::Painter, x: f32| {
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(x - s * 0.8, c.y - s),
                Pos2::new(x - s * 0.8, c.y + s),
                Pos2::new(x + s, c.y),
            ],
            col,
            egui::Stroke::NONE,
        ));
    };
    match icon {
        Icon::Play => tri(p, c.x + 1.0),
        Icon::Pause => {
            for dx in [-4.0, 4.0] {
                p.rect_filled(
                    Rect::from_center_size(Pos2::new(c.x + dx, c.y), Vec2::new(4.0, 2.0 * s)),
                    1.0,
                    col,
                );
            }
        }
        Icon::Stop => {
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(1.7 * s)), 1.5, col);
        }
        Icon::ToIn => {
            p.rect_filled(
                Rect::from_center_size(Pos2::new(c.x - s, c.y), Vec2::new(3.0, 2.0 * s)),
                1.0,
                col,
            );
            p.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x + s, c.y - s),
                    Pos2::new(c.x + s, c.y + s),
                    Pos2::new(c.x - s * 0.7, c.y),
                ],
                col,
                egui::Stroke::NONE,
            ));
        }
    }
    resp
}

/// Width shared by Cart Setup's drop-downs; Output alone spans the column.
const FORM_COMBO_W: f32 = 190.0;

/// A form row: left-aligned label in a fixed column, then the controls. Drop-downs
/// share one width, so fade in and fade out (and the rest) line up.
fn form_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(26.0);
        field_label(ui, label);
        ui.spacing_mut().combo_width = FORM_COMBO_W;
        add(ui);
    });
}

/// Seconds for the speed bar's ramps (x0 slow-down, x1 speed-up; x1 may be instant).
fn seconds_combo(ui: &mut egui::Ui, salt: &str, ms: &mut u32, instant: bool) {
    let mut choices: Vec<u32> = vec![500, 1000, 1500, 2000, 3000, 4000, 5000, 8000];
    if instant {
        choices.insert(0, 0);
    }
    let text = |v: u32| {
        if v == 0 {
            "Instant".to_string()
        } else {
            format!("{:.1} s", v as f32 / 1000.0)
        }
    };
    egui::ComboBox::from_id_salt(salt)
        .width(78.0)
        .selected_text(text(*ms))
        .show_ui(ui, |ui| {
            for v in choices {
                ui.selectable_value(ms, v, text(v));
            }
        });
}

/// Trim In / Out box of an exact size: ◀ time ▶ centred, Clear beneath.
#[allow(clippy::too_many_arguments)]
fn trim_box_sized(
    ui: &mut egui::Ui,
    title: &str,
    t: &mut f64,
    min: f64,
    max: f64,
    nudge: f64,
    changed: &mut bool,
    reset: f64,
    width: f32,
    height: f32,
) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(width - 16.0);
            ui.set_height(height - 16.0);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.label(egui::RichText::new(title).weak());
                ui.horizontal(|ui| {
                    ui.add_space(((width - 16.0) - (30.0 + 110.0 + 30.0 + 12.0)).max(0.0) / 2.0);
                    if ui
                        .add_sized([30.0, 26.0], egui::Button::new("◀"))
                        .on_hover_text("Earlier by 10 ms (Shift: 100 ms)")
                        .clicked()
                    {
                        *t = (*t - nudge).max(min);
                        *changed = true;
                    }
                    *changed |= ui
                        .add_sized(
                            [110.0, 26.0],
                            egui::DragValue::new(t)
                                .range(min..=max)
                                .speed(0.01)
                                .custom_formatter(|v, _| fmt_precise(v as f32))
                                .custom_parser(parse_time),
                        )
                        .changed();
                    if ui
                        .add_sized([30.0, 26.0], egui::Button::new("▶"))
                        .on_hover_text("Later by 10 ms (Shift: 100 ms)")
                        .clicked()
                    {
                        *t = (*t + nudge).min(max);
                        *changed = true;
                    }
                });
                if ui
                    .add_sized([80.0, 22.0], egui::Button::new("Clear"))
                    .clicked()
                {
                    *t = reset;
                    *changed = true;
                }
            });
        });
}

/// "mm:ss.s" or seconds → seconds.
fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.split_once(':') {
        Some((m, sec)) => {
            Some(m.trim().parse::<f64>().ok()? * 60.0 + sec.trim().parse::<f64>().ok()?)
        }
        None => s.parse().ok(),
    }
}

/// Fade lengths offered in the fade pickers (ms): zero, 0.1–0.9 s, 1–5 s by 0.5 s.
const FADE_PRESETS_MS: [u32; 19] = [
    0, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1500, 2000, 2500, 3000, 3500, 4000, 4500,
    5000,
];

fn fade_text(ms: u32) -> String {
    if ms == 0 {
        "zero".into()
    } else {
        format!("{:.1} s", ms as f32 / 1000.0)
    }
}

/// A fade length: a list of common values plus a seconds field for anything else.
/// With `default_ms`, `None` means "the session default" (fade-out); without it,
/// `None` means zero (fade-in).
fn fade_picker(
    ui: &mut egui::Ui,
    salt: &str,
    fade: &mut Option<FadeSpec>,
    default_ms: Option<u32>,
) {
    fade_picker_ex(ui, salt, fade, default_ms, true);
}

/// As [`fade_picker`]; `field` adds the seconds box for other lengths.
fn fade_picker_ex(
    ui: &mut egui::Ui,
    salt: &str,
    fade: &mut Option<FadeSpec>,
    default_ms: Option<u32>,
    field: bool,
) {
    let curve = fade.map_or(Curve::EqualPower, |f| f.curve);
    let set = |ms: u32| {
        Some(FadeSpec {
            curve,
            ..FadeSpec::ms(ms)
        })
    };
    let current = fade.map(|f| f.ms);
    let shown = match (current, default_ms) {
        (None, Some(d)) => format!("Default ({})", fade_text(d)),
        (None, None) => fade_text(0),
        (Some(ms), _) => fade_text(ms),
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(salt)
            .selected_text(shown)
            .width(if field {
                110.0
            } else {
                ui.spacing().combo_width
            })
            .show_ui(ui, |ui| {
                if let Some(d) = default_ms
                    && ui
                        .selectable_label(current.is_none(), format!("Default ({})", fade_text(d)))
                        .clicked()
                {
                    *fade = None;
                }
                for ms in FADE_PRESETS_MS {
                    let on = current == Some(ms)
                        || (default_ms.is_none() && ms == 0 && current.is_none());
                    if ui.selectable_label(on, fade_text(ms)).clicked() {
                        *fade = if ms == 0 && default_ms.is_none() {
                            None
                        } else {
                            set(ms)
                        };
                    }
                }
            });
        if !field {
            return;
        }
        let mut secs = current.or(default_ms).unwrap_or(0) as f32 / 1000.0;
        if ui
            .add(
                egui::DragValue::new(&mut secs)
                    .range(0.0..=60.0)
                    .speed(0.05)
                    .max_decimals(2)
                    .suffix(" s"),
            )
            .on_hover_text("Any other length, in seconds")
            .changed()
        {
            let ms = (secs * 1000.0).round() as u32;
            *fade = if ms == 0 && default_ms.is_none() {
                None
            } else {
                set(ms)
            };
        }
    });
}

/// Output picker: `labels[i]` describes output i (A, B, …).
fn output_combo(ui: &mut egui::Ui, salt: &str, labels: &[String], output: &mut String) {
    let current = bubble_project::output_index(output).unwrap_or(0);
    egui::ComboBox::from_id_salt(salt)
        .selected_text(
            labels
                .get(current)
                .cloned()
                .unwrap_or_else(|| output.clone()),
        )
        .show_ui(ui, |ui| {
            for (i, l) in labels.iter().enumerate() {
                if ui.selectable_label(i == current, l).clicked() {
                    *output = bubble_project::output_letter(i);
                }
            }
        });
}

/// Exclusive group picker showing "G1" / "G1 Name".
fn group_combo(ui: &mut egui::Ui, salt: &str, project: &Project, group: &mut Option<Uuid>) {
    let current = group
        .and_then(|g| project.groups.iter().position(|x| x.id == g))
        .map(|i| project.groups[i].label(i))
        .unwrap_or_else(|| "none".into());
    egui::ComboBox::from_id_salt(salt)
        .selected_text(current)
        .show_ui(ui, |ui| {
            ui.selectable_value(group, None, "none");
            for (i, g) in project.groups.iter().enumerate() {
                ui.selectable_value(group, Some(g.id), g.label(i));
            }
        });
}

/// Quick colour choices plus a colour-wheel button that opens the full picker
/// inline (`open` keeps it open between frames). `None` = default colour.
fn colour_swatches(ui: &mut egui::Ui, color: &mut Option<String>, open: &mut bool) {
    let current = parse_color(color.as_deref()).unwrap_or(DEFAULT_CART_COLOR);
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for sw in SWATCHES {
                let col = Color32::from_rgb(sw[0], sw[1], sw[2]);
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                ui.painter().rect_filled(r, 4.0, col);
                if current == col {
                    ui.painter().rect_stroke(
                        r.expand(1.5),
                        5.0,
                        egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
                        egui::StrokeKind::Outside,
                    );
                }
                if resp.clicked() {
                    *color = (sw != [0x3A, 0x6E, 0xA5]).then(|| hex(sw));
                }
            }
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
            paint_colour_wheel(ui.painter(), r, current);
            if resp.on_hover_text("More colours").clicked() {
                *open = !*open;
            }
        });
        if *open {
            let mut c = current;
            if egui::color_picker::color_picker_color32(
                ui,
                &mut c,
                egui::color_picker::Alpha::Opaque,
            ) {
                *color = Some(hex([c.r(), c.g(), c.b()]));
            }
        }
    });
}

/// A small hue wheel with the current colour in the middle (vector, any size).
fn paint_colour_wheel(painter: &egui::Painter, r: Rect, current: Color32) {
    let c = r.center();
    let radius = r.width().min(r.height()) / 2.0;
    let n = 36;
    let mut mesh = egui::Mesh::default();
    for i in 0..n {
        let a0 = i as f32 / n as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / n as f32 * std::f32::consts::TAU;
        let hue = egui::ecolor::Hsva::new(i as f32 / n as f32, 0.85, 1.0, 1.0);
        let col: Color32 = hue.into();
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(c, col);
        mesh.colored_vertex(c + Vec2::angled(a0) * radius, col);
        mesh.colored_vertex(c + Vec2::angled(a1) * radius, col);
        mesh.add_triangle(base, base + 1, base + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
    painter.circle_filled(c, radius * 0.45, current);
    painter.circle_stroke(c, radius * 0.45, egui::Stroke::new(1.5, Color32::WHITE));
}

fn key_code(k: Key) -> String {
    let name = k.name();
    if name.len() == 1 {
        let ch = name.chars().next().unwrap_or(' ');
        if ch.is_ascii_alphabetic() {
            return format!("Key{}", ch.to_ascii_uppercase());
        }
        if ch.is_ascii_digit() {
            return format!("Digit{ch}");
        }
    }
    name.to_string()
}

fn modifier_names(m: egui::Modifiers) -> Vec<String> {
    let mut v = Vec::new();
    if m.alt {
        v.push("Alt".to_string());
    }
    if m.ctrl {
        v.push("Ctrl".to_string());
    }
    if m.mac_cmd || (m.command && !m.ctrl) {
        v.push("Meta".to_string());
    }
    if m.shift {
        v.push("Shift".to_string());
    }
    v.sort();
    v
}

impl eframe::App for BubbleApp {
    /// Runs on every shutdown path, including a system quit (Dock, AppleScript,
    /// logout) that bypasses the unsaved-changes prompt.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.window_moved.is_some() {
            let _ = self.prefs.save();
        }
        if self.discard_on_exit {
            self.remove_autosave();
        } else {
            self.write_autosave();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.track_window(&ctx);
        // For testing crash handling: BUBBLE_CRASH_TEST=1 panics a few seconds in.
        if std::env::var_os("BUBBLE_CRASH_TEST").is_some()
            && self.started.elapsed() > Duration::from_secs(3)
        {
            panic!("crash test requested by BUBBLE_CRASH_TEST");
        }
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            crate::log::info("first frame drawn");
        }
        self.watch_audio(&ctx);
        self.poll_open();
        let snap = self.snapshot();
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
        let mut intents = Vec::new();
        if let Some(p) = self.open_at_start.take() {
            self.open_any(p, &snap, &mut intents);
        }
        self.fonts
            .apply(&ctx, snap.project.settings.display.font.as_deref());
        let dt = self.last_frame.elapsed().as_secs_f32();
        self.last_frame = Instant::now();
        self.update_meters(&snap, dt);
        self.child_focus = false;
        // Before poll_job, so audio a finished job adds is in the project by now.
        self.wav_check_when_idle();
        self.poll_job(&mut intents);
        if self.want_collect && self.job.is_none() && self.opening.is_none() {
            self.want_collect = false;
            self.collect_external();
        }
        self.update_title(&ctx, &snap);

        self.file_results(&snap, &mut intents);
        self.keyboard(&ctx, &snap, &mut intents);
        self.drop_files(&ctx, &snap, &mut intents);

        let model = self.build_menu(&snap);
        let mut cmds: Vec<Cmd> = Vec::new();
        #[cfg(target_os = "macos")]
        {
            self.native_menu.update(&model);
            cmds.extend(self.native_menu.poll());
            if self.native_menu.error.is_some() {
                crate::menu::egui_shortcuts(&ctx, &model, &mut cmds);
                egui::Panel::top("menu")
                    .show(ui, |ui| crate::menu::egui_menu_bar(ui, &model, &mut cmds));
            }
        }
        // Windows and Linux: the in-window menu bar, which follows light/dark.
        #[cfg(not(target_os = "macos"))]
        {
            crate::menu::egui_shortcuts(&ctx, &model, &mut cmds);
            egui::Panel::top("menu")
                .show(ui, |ui| crate::menu::egui_menu_bar(ui, &model, &mut cmds));
        }
        let _ = frame;
        for cmd in cmds {
            self.handle_cmd(cmd, &ctx, &snap, &mut intents);
        }
        self.progress_banner(ui, &snap);
        let bar_h = match self.prefs.bar_size {
            0 => 44.0,
            1 => 64.0,
            _ => 92.0,
        };
        egui::Panel::bottom("pagebar")
            .exact_size(bar_h)
            .resizable(false)
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill))
            .show(ui, |ui| self.bottom_bar(ui, &snap, &mut intents));
        egui::CentralPanel::default().show(ui, |ui| self.grid(ui, &snap, &mut intents));
        self.quick_edit_popup(&ctx, &snap, &mut intents);
        self.windows(&ctx, &snap, &mut intents);
        self.close_guard(&ctx, &snap, &mut intents);

        // Key releases cannot be seen while Bubble is in the background, so a held
        // hold-to-play cart would never stop: release them when focus leaves the app.
        let focused = ctx.input(|i| i.focused) || self.child_focus;
        if self.had_focus && !focused {
            intents.push(Intent::ReleaseHolds);
        }
        self.had_focus = focused;

        self.apply(intents);
        self.files.launch();
        self.autosave();

        let playing = snap
            .runtime
            .values()
            .any(|r| matches!(r.state, CartState::Playing | CartState::Stopping));
        let metering = self.meters.values().any(|m| !m.is_idle());
        // Redraw only while something moves. Idle, a slow tick keeps autosave and
        // the clock-based status going; on a machine drawing in software every frame
        // costs real CPU, so an idle window must not redraw several times a second.
        if playing || metering {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else if snap.pending > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.midi.active_recently(150) {
            // Clears the MIDI activity light.
            ctx.request_repaint_after(Duration::from_millis(200));
        } else {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
    }
}

impl BubbleApp {
    fn keyboard(&mut self, ctx: &egui::Context, snap: &Snapshot, intents: &mut Vec<Intent>) {
        // Text fields keep their key presses; otherwise every key goes to the binding
        // resolver. Releases are always forwarded so a hold-to-play cart never sticks.
        let typing = ctx.egui_wants_keyboard_input() && snap.learning.is_none();
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            if let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } = ev
            {
                if typing && pressed {
                    continue;
                }
                let k = physical_key.unwrap_or(key);
                // Edit mode, main window: arrows move the cart cursor, Return opens
                // quick edit. They are not passed to triggers there.
                if self.edit_mode
                    && ctx.viewport_id() == egui::ViewportId::ROOT
                    && self.quick_edit.is_none()
                    && !modifiers.command
                {
                    let step = match k {
                        Key::ArrowLeft => Some((0i32, -1i32)),
                        Key::ArrowRight => Some((0, 1)),
                        Key::ArrowUp => Some((-1, 0)),
                        Key::ArrowDown => Some((1, 0)),
                        _ => None,
                    };
                    if let Some((dr, dc)) = step {
                        if pressed {
                            self.move_cursor(snap, dr, dc);
                        }
                        continue;
                    }
                    if k == Key::Enter && snap.learning.is_none() {
                        if pressed && let Some(id) = self.selected {
                            self.open_quick_edit_at(ctx, snap, id);
                        }
                        continue;
                    }
                }
                // Esc closes the quick-edit popup rather than stopping everything.
                if k == Key::Escape && self.quick_edit.is_some() {
                    continue;
                }
                // App shortcuts with Cmd/Ctrl are handled by the menu.
                if modifiers.command
                    && matches!(
                        k,
                        Key::S
                            | Key::O
                            | Key::N
                            | Key::Z
                            | Key::X
                            | Key::C
                            | Key::V
                            | Key::E
                            | Key::I
                            | Key::D
                            | Key::L
                            | Key::Q
                            | Key::W
                            | Key::Comma
                            | Key::Period
                    )
                {
                    continue;
                }
                intents.push(Intent::Input(InputEvent::Key {
                    code: key_code(k),
                    modifiers: modifier_names(modifiers),
                    pressed,
                    repeat,
                }));
            }
        }
        #[cfg(target_os = "macos")]
        let (save, open, new) = (false, false, false);
        #[cfg(not(target_os = "macos"))]
        let (save, open, new) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, Key::S),
                i.consume_key(egui::Modifiers::COMMAND, Key::O),
                i.consume_key(egui::Modifiers::COMMAND, Key::N),
            )
        });
        if save {
            self.save_or_save_as(snap, intents);
        }
        if open {
            self.files.request(Purpose::Open);
        }
        if new {
            self.request(PendingAction::New, snap, intents);
        }
    }

    fn request(&mut self, a: PendingAction, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if snap.dirty {
            self.dialogs.confirm_discard = Some(a);
        } else {
            self.perform(a, intents);
        }
    }

    fn perform(&mut self, a: PendingAction, intents: &mut Vec<Intent>) {
        match a {
            PendingAction::New => self.begin_new_session(String::new(), AfterCreate::Empty),
            PendingAction::Open(p) => intents.push(Intent::Open(p)),
            PendingAction::Import(p) => {
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().trim_start_matches('~').to_string())
                    .unwrap_or_default();
                self.begin_new_session(stem, AfterCreate::Import(p));
            }
        }
    }

    /// Where the pointer is during an OS file drag, in window points. winit 0.30
    /// reports dragged files without a position and macOS sends no mouse moves during
    /// a drag, so the system cursor is read and mapped into the window.
    fn drag_pos(ctx: &egui::Context) -> Option<Pos2> {
        // egui's own pointer position is stale during an OS drag (it is wherever the
        // mouse was before the drag), so always ask the system.
        let inner = ctx.input(|i| i.viewport().inner_rect)?;
        match mouse_position::mouse_position::Mouse::get_mouse_position() {
            mouse_position::mouse_position::Mouse::Position { x, y } => {
                // macOS reports points; Windows and X11 report pixels.
                let scale = if cfg!(target_os = "macos") {
                    1.0
                } else {
                    ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(1.0))
                };
                let global = Pos2::new(x as f32 / scale, y as f32 / scale);
                Some((global - inner.min.to_vec2()) / ctx.zoom_factor())
            }
            mouse_position::mouse_position::Mouse::Error => None,
        }
    }

    fn drop_files(&mut self, ctx: &egui::Context, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        self.drag_hover = if hovering || !dropped.is_empty() {
            Self::drag_pos(ctx)
        } else {
            None
        };
        if hovering {
            // No events arrive while files are dragged over the window: keep redrawing
            // so the highlight follows the pointer.
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if dropped.is_empty() {
            return;
        }
        // A session, SpotOn session or package opens (or imports).
        if let Some(p) = dropped.iter().find(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| OPEN_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        }) {
            self.open_any(p.clone(), snap, intents);
            return;
        }
        let mut files: Vec<PathBuf> = dropped
            .into_iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            })
            .collect();
        files.sort();
        let target = self
            .drag_hover
            .and_then(|p| {
                ctx.data(|d| d.get_temp::<Vec<(Rect, u16, u16)>>(egui::Id::unique("cells")))
                    .and_then(|cells| cells.into_iter().find(|(r, _, _)| r.contains(p)))
            })
            .map(|(_, r, c)| (r, c));
        self.drag_hover = None;
        if files.is_empty() {
            return;
        }
        match target {
            Some((r, c)) => self.assign_into_session(snap.page, r, c, files),
            // Dropped outside the grid: fill from the first empty cell.
            None => {
                if let Some((p, r, c)) = self.first_empty_cell(snap) {
                    self.assign_into_session(p, r, c, files);
                }
            }
        }
    }

    /// Window title: the session and whether it is saved, as in the reference.
    fn update_title(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let title = match &snap.path {
            None => "Bubble - No session".to_string(),
            Some(_) => {
                let saved = self.saved_at.map(session_ui::saved_text);
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
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    /// Duplicates a cart into the next empty cell after it on its page.
    fn duplicate_cart(&mut self, id: Uuid, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let Some((p, r, c)) = snap.project.cart_position(id) else {
            return;
        };
        let page = &snap.project.pages[p];
        let start = r as u32 * page.cols as u32 + c as u32 + 1;
        let total = page.rows as u32 * page.cols as u32;
        let free = (start..total)
            .chain(0..start)
            .map(|i| ((i / page.cols as u32) as u16, (i % page.cols as u32) as u16))
            .find(|(r, c)| page.cell(*r, *c).is_none());
        let clip = self.lock().copy_cart(id);
        match (free, clip) {
            (Some((nr, nc)), Some(clip)) => intents.push(Intent::Paste(p, nr, nc, clip)),
            (None, _) => self.dialogs.error = Some("This page is full.".into()),
            _ => {}
        }
    }

    /// Moves the keyboard cursor one cell (wrapping across rows).
    fn move_cursor(&mut self, snap: &Snapshot, dr: i32, dc: i32) {
        let Some(page) = snap.project.pages.get(snap.page) else {
            return;
        };
        let (rows, cols) = (page.rows as i32, page.cols as i32);
        let (r, c) = match self
            .selected
            .and_then(|id| snap.project.cart_position(id))
            .filter(|(p, _, _)| *p == snap.page)
            .or(self.cursor_empty.filter(|(p, _, _)| *p == snap.page))
        {
            Some((_, r, c)) => (r as i32, c as i32),
            None => {
                self.cursor_empty = None;
                self.selected = page.cell(0, 0).map(|c| c.cart.id);
                if self.selected.is_none() {
                    self.cursor_empty = Some((snap.page, 0, 0));
                }
                return;
            }
        };
        let idx = (r * cols + c + dr * cols + dc).rem_euclid(rows * cols);
        let (nr, nc) = ((idx / cols) as u16, (idx % cols) as u16);
        match page.cell(nr, nc) {
            Some(cell) => {
                self.selected = Some(cell.cart.id);
                self.cursor_empty = None;
            }
            None => {
                self.selected = None;
                self.cursor_empty = Some((snap.page, nr, nc));
            }
        }
    }

    /// Opens quick edit beside a cart's tile.
    fn open_quick_edit_at(&mut self, ctx: &egui::Context, snap: &Snapshot, id: Uuid) {
        let Some((p, r, c)) = snap.project.cart_position(id) else {
            return;
        };
        if p != snap.page {
            return;
        }
        let at = ctx
            .data(|d| d.get_temp::<Vec<(Rect, u16, u16)>>(egui::Id::unique("cells")))
            .and_then(|cells| cells.into_iter().find(|(_, rr, cc)| (*rr, *cc) == (r, c)))
            .map_or(Pos2::new(200.0, 200.0), |(rect, _, _)| rect.center());
        self.quick_edit = Some((id, at));
    }

    fn first_empty_cell(&self, snap: &Snapshot) -> Option<(usize, u16, u16)> {
        let page = snap.project.pages.get(snap.page)?;
        (0..page.rows)
            .flat_map(|r| (0..page.cols).map(move |c| (r, c)))
            .find(|(r, c)| page.cell(*r, *c).is_none())
            .map(|(r, c)| (snap.page, r, c))
    }

    fn build_menu(&self, snap: &Snapshot) -> Menu {
        let s = &snap.project.settings;
        let sel = self.selected.filter(|id| snap.project.cart(*id).is_some());
        let has_sel = sel.is_some();
        let target_cell = self.target_cell(snap);
        let recent: Vec<Item> = self
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
                .enabled(self.clipboard.is_some() && target_cell.is_some()),
            Item::action("Paste Attributes…", Cmd::PasteAttributes)
                .accel("CmdOrCtrl+Shift+V")
                .enabled(self.clipboard.is_some() && has_sel),
            Item::action("Duplicate Cart", Cmd::DuplicateCart)
                .accel("CmdOrCtrl+D")
                .enabled(has_sel),
            Item::action("Clear Cart", Cmd::ClearCart).enabled(has_sel),
            Item::Separator,
            Item::action("Copy Page", Cmd::CopyPage),
            Item::action("Paste Page", Cmd::PastePage).enabled(self.page_clip.is_some()),
            Item::Separator,
            Item::action("Edit Mode", Cmd::ToggleEditMode)
                .accel("CmdOrCtrl+E")
                .check(self.edit_mode),
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

        let rate = self.prefs.rate();
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
            .check(self.prefs.buffer_frames == b)
        })
        .collect();
        let mut midi_items = vec![
            Item::action("Enable MIDI Input", Cmd::MidiEnabled).check(!self.prefs.midi_disabled),
            Item::action("All Ports", Cmd::MidiAllPorts).check(self.prefs.midi_inputs.is_empty()),
            Item::Separator,
        ];
        for port in &self.midi_ports {
            midi_items.push(
                Item::action(port.clone(), Cmd::MidiPort(port.clone()))
                    .check(self.prefs.midi_inputs.contains(port))
                    .enabled(!self.prefs.midi_disabled),
            );
        }
        if self.midi_ports.is_empty() {
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
        let zoom = (self.prefs.ui_scale.unwrap_or(1.0) * 100.0).round() as u32;
        let view = vec![
            Item::sub(
                "Page Bar Size",
                ["Small", "Medium", "Large (touch screen)"]
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| {
                        Item::action(n, Cmd::BarSize(i as u8)).check(self.prefs.bar_size == i as u8)
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
            Item::sub("Appearance", {
                use crate::prefs::Appearance as A;
                [
                    ("Follow System", A::System),
                    ("Light", A::Light),
                    ("Dark", A::Dark),
                ]
                .into_iter()
                .map(|(n, a)| Item::action(n, Cmd::Appearance(a)).check(self.prefs.appearance == a))
                .collect()
            }),
            Item::Separator,
            Item::action("Playout Log", Cmd::ShowLog).accel("CmdOrCtrl+L"),
            Item::action("Audio Diagnostics", Cmd::ShowDiagnostics),
            Item::action("Show Log File", Cmd::ShowLogs),
        ];
        Menu {
            tops: vec![
                Top {
                    title: "File".into(),
                    items: file,
                },
                Top {
                    title: "Edit".into(),
                    items: edit,
                },
                Top {
                    title: "Session".into(),
                    items: session,
                },
                Top {
                    title: "Cart".into(),
                    items: cart,
                },
                Top {
                    title: "Audio".into(),
                    items: audio,
                },
                Top {
                    title: "View".into(),
                    items: view,
                },
            ],
        }
    }

    /// The cell Paste targets: the selected cart's cell, else the first empty cell of the page.
    fn target_cell(&self, snap: &Snapshot) -> Option<(usize, u16, u16)> {
        if let Some(pos) = self.selected.and_then(|id| snap.project.cart_position(id)) {
            return Some(pos);
        }
        if let Some(pos) = self.cursor_empty.filter(|(p, _, _)| *p == snap.page) {
            return Some(pos);
        }
        let page = snap.project.pages.get(snap.page)?;
        (0..page.rows)
            .flat_map(|r| (0..page.cols).map(move |c| (r, c)))
            .find(|(r, c)| page.cell(*r, *c).is_none())
            .map(|(r, c)| (snap.page, r, c))
    }

    fn handle_cmd(
        &mut self,
        cmd: Cmd,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        use bubble_project::{DuplicatePolicy as D, MouseMode as M, Scope as S};
        let mut settings = snap.project.settings.clone();
        let sel = self.selected.filter(|id| snap.project.cart(*id).is_some());
        match cmd {
            Cmd::New => self.request(PendingAction::New, snap, intents),
            Cmd::Open => self.files.request(Purpose::Open),
            Cmd::OpenRecent(i) => {
                if let Some(p) = self.prefs.recent_projects.get(i).cloned() {
                    self.request(PendingAction::Open(p), snap, intents);
                }
            }
            Cmd::Save => self.save_or_save_as(snap, intents),
            Cmd::SaveAs => {
                self.begin_new_session(format!("{} copy", snap.project.name), AfterCreate::SaveAs)
            }
            Cmd::Relink => self.dialogs.relink = true,
            Cmd::ReloadAudio => intents.push(Intent::Refresh),
            Cmd::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Undo => intents.push(Intent::Undo),
            Cmd::Redo => intents.push(Intent::Redo),
            Cmd::Copy | Cmd::Cut => {
                if let Some(id) = sel {
                    let clip = self.lock().copy_cart(id);
                    self.clipboard = clip;
                    if cmd == Cmd::Cut
                        && let Some((p, r, c)) = snap.project.cart_position(id)
                    {
                        intents.push(Intent::ClearCell(p, r, c));
                        self.selected = None;
                    }
                }
            }
            Cmd::Paste => {
                if let (Some(clip), Some((p, r, c))) =
                    (self.clipboard.clone(), self.target_cell(snap))
                {
                    intents.push(Intent::Paste(p, r, c, clip));
                }
            }
            Cmd::PasteAttributes => self.dialogs.paste_attrs = self.clipboard.is_some(),
            Cmd::ClearCart => {
                if let Some((p, r, c)) = sel.and_then(|id| snap.project.cart_position(id)) {
                    intents.push(Intent::ClearCell(p, r, c));
                    self.selected = None;
                }
            }
            Cmd::ToggleEditMode => self.edit_mode = !self.edit_mode,
            Cmd::AddPage => intents.push(Intent::AddPage),
            Cmd::RemovePage => intents.push(Intent::RemovePage(snap.page)),
            Cmd::AudioSetup => {
                if let Some(id) = sel {
                    self.open_audio_setup(id);
                }
            }
            Cmd::PlaySelected => {
                if let Some(id) = sel {
                    intents.push(Intent::Trigger(id));
                }
            }
            Cmd::StopAll => intents.push(Intent::StopAll(true)),
            Cmd::StopPage => intents.push(Intent::StopPage(snap.page)),
            Cmd::StopAllNow => intents.push(Intent::StopAll(false)),
            Cmd::Outputs => {
                self.open_outputs_window();
                crate::native::raise(ctx, "outputs");
            }
            Cmd::ReassignOutputs => {
                if self.dialogs.reassign.is_none() {
                    let cells = snap
                        .project
                        .pages
                        .get(snap.page)
                        .map_or(1, |p| p.rows as u32 * p.cols as u32);
                    self.dialogs.reassign = Some(outputs_ui::ReassignDraft {
                        from: None,
                        to: 0,
                        scope: 0,
                        group: None,
                        first: 1,
                        last: cells,
                    });
                }
                crate::native::raise(ctx, "reassign");
            }
            Cmd::ShowLogs => {
                if let Some(d) = crate::log::dir() {
                    reveal(&d.join("bubble.log"));
                }
            }
            Cmd::RevealSession => {
                if let Some(p) = &snap.path {
                    reveal(p);
                }
            }
            Cmd::CopyPage => {
                let clip = self.lock().copy_page(snap.page);
                self.page_clip = clip;
            }
            Cmd::PastePage => {
                if let Some(clip) = self.page_clip.clone() {
                    intents.push(Intent::PastePage(snap.page, clip));
                }
            }
            Cmd::DuplicateCart => {
                if let Some(id) = sel {
                    self.duplicate_cart(id, snap, intents);
                }
            }
            Cmd::SampleRate(r) => {
                self.prefs.sample_rate = r;
                let _ = self.prefs.save();
                self.open_audio();
            }
            Cmd::BufferFrames(b) => {
                self.prefs.buffer_frames = b;
                let _ = self.prefs.save();
                self.open_audio();
            }
            Cmd::MidiEnabled => {
                self.prefs.midi_disabled = !self.prefs.midi_disabled;
                self.reopen_midi();
            }
            Cmd::MidiAllPorts => {
                self.prefs.midi_inputs.clear();
                self.reopen_midi();
            }
            Cmd::MidiPort(name) => {
                if let Some(i) = self.prefs.midi_inputs.iter().position(|n| *n == name) {
                    self.prefs.midi_inputs.remove(i);
                } else {
                    self.prefs.midi_inputs.push(name);
                }
                self.reopen_midi();
            }
            Cmd::RescanMidi => {
                self.midi_ports = available_ports();
                self.reopen_midi();
            }
            Cmd::MouseToggle => settings.mouse_mode = M::Toggle,
            Cmd::MouseLeftRight => settings.mouse_mode = M::LeftPlayRightStop,
            Cmd::DuplicatesGanged => settings.duplicate_binding_policy = D::Ganged,
            Cmd::DuplicatesOverlapped => settings.duplicate_binding_policy = D::Overlapped,
            Cmd::ScopeGlobal => settings.binding_scope = S::Global,
            Cmd::ScopePage => settings.binding_scope = S::CurrentPage,
            Cmd::Limiter => settings.limiter = !settings.limiter,
            Cmd::ShowLog => {
                self.dialogs.log = true;
                crate::native::raise(ctx, "log");
            }
            Cmd::ShowDiagnostics => {
                self.dialogs.diagnostics = true;
                crate::native::raise(ctx, "diagnostics");
            }
            Cmd::OutputLevel(db) => settings.master_gain_db = db as f32,
            Cmd::ProjectSettings => {
                self.dialogs.project_settings = true;
                crate::native::raise(ctx, "project-settings");
            }
            Cmd::PageSettings => {
                self.dialogs.page_settings = Some(snap.page);
                crate::native::raise(ctx, "page-settings");
            }
            Cmd::SpeedBar => {
                if let Some(id) = sel {
                    self.open_speed_bar(id);
                }
            }
            Cmd::BarSize(b) => {
                self.prefs.bar_size = b.min(2);
                let _ = self.prefs.save();
            }
            Cmd::Appearance(a) => {
                self.prefs.appearance = a;
                ctx.set_theme(a);
                let _ = self.prefs.save();
            }
            Cmd::Zoom(z) => {
                let f = z as f32 / 100.0;
                ctx.set_zoom_factor(f);
                self.prefs.ui_scale = Some(f);
                let _ = self.prefs.save();
            }
        }
        if settings != snap.project.settings {
            intents.push(Intent::Settings(settings));
        }
    }

    fn reopen_midi(&mut self) {
        let _ = self.prefs.save();
        self.midi = MidiInputs::empty();
        self.open_midi();
    }

    /// Up to four speed bars at once (as in the reference); the oldest makes way.
    fn open_speed_bar(&mut self, cart: Uuid) {
        if !self.dialogs.speed_bars.contains(&cart) {
            if self.dialogs.speed_bars.len() >= 4 {
                self.dialogs.speed_bars.remove(0);
            }
            self.dialogs.speed_bars.push(cart);
        }
    }

    /// PPM ballistics for carts whose meters are visible.
    fn update_meters(&mut self, snap: &Snapshot, dt: f32) {
        let visible: Vec<Uuid> = self.dialogs.audio_setup.into_iter().collect();
        self.meters.retain(|id, _| visible.contains(id));
        for id in visible {
            let peak = snap.peaks.get(&id).copied().unwrap_or_default();
            self.meters.entry(id).or_default().update(peak, dt);
        }
    }

    fn open_audio_setup(&mut self, cart: Uuid) {
        self.focus_setup = true;
        if self.dialogs.audio_setup != Some(cart) {
            self.wave_view = WaveView::default();
        }
        self.dialogs.audio_setup = Some(cart);
    }

    /// Bottom bar: Edit/Show toggle, page tabs (drag to reorder, double-click to rename,
    /// right-click for more), a status line when there is news, Stop Page and STOP ALL.
    /// Buttons are placed at exact rects so nothing is clipped at any bar height.
    fn bottom_bar(&mut self, ui: &mut egui::Ui, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let bar = ui.max_rect();
        let m = 5.0;
        let gap = 6.0;
        let btn_h = (bar.height() - 2.0 * m).max(20.0);
        let text = (btn_h * 0.36).clamp(12.0, 30.0);
        let y0 = bar.top() + m;
        let multi = snap.project.pages.len() > 1;
        let stop_w = (btn_h * 3.2).max(120.0);
        let page_w = (btn_h * 2.4).max(96.0);
        let neutral = ui.visuals().widgets.inactive.weak_bg_fill;

        // Right: STOP ALL, then Stop Page (always shown).
        let mut right = bar.right() - m;
        let stop_rect =
            Rect::from_min_size(Pos2::new(right - stop_w, y0), Vec2::new(stop_w, btn_h));
        right = stop_rect.left() - gap;
        let stop = egui::Button::new(
            egui::RichText::new("■ STOP ALL")
                .size(text * 1.1)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(Color32::from_rgb(0xB0, 0x20, 0x20));
        if ui
            .put(stop_rect, stop)
            .on_hover_text("Stops everything immediately, without fades (Esc fades out)")
            .clicked()
        {
            intents.push(Intent::StopAll(false));
        }
        let sp = Rect::from_min_size(Pos2::new(right - page_w, y0), Vec2::new(page_w, btn_h));
        right = sp.left() - gap;
        if ui
            .put(
                sp,
                egui::Button::new(
                    egui::RichText::new("■ STOP PAGE")
                        .size(text * 0.95)
                        .strong(),
                )
                .fill(neutral),
            )
            .on_hover_text("Fade out the carts on this page")
            .clicked()
        {
            intents.push(Intent::StopPage(snap.page));
        }

        // Left: Edit/Show toggle.
        let mut x = bar.left() + m;
        let (mode, col) = if self.edit_mode {
            ("EDIT", Color32::from_rgb(0xF0, 0x9A, 0x2A))
        } else {
            ("SHOW", Color32::from_rgb(0x3C, 0xB3, 0x71))
        };
        let mr = Rect::from_min_size(Pos2::new(x, y0), Vec2::new((btn_h * 1.5).max(56.0), btn_h));
        x = mr.right() + gap * 2.0;
        if ui
            .put(
                mr,
                egui::Button::new(
                    egui::RichText::new(mode)
                        .size(text * 0.8)
                        .strong()
                        .color(Color32::WHITE),
                )
                .fill(col),
            )
            .on_hover_text("Edit mode (⌘E). Show mode locks editing: clicks only play and stop.")
            .clicked()
        {
            self.edit_mode = !self.edit_mode;
        }

        // Status line (only when there is something to say), measured first so the
        // tabs know how much room they have.
        let mut status: Vec<(String, Color32)> = Vec::new();
        let warn = Color32::from_rgb(0xF0, 0x9A, 0x2A);
        if let Some(e) = &self.audio_error {
            status.push((
                format!("No audio: {e}"),
                Color32::from_rgb(0xE5, 0x39, 0x35),
            ));
        } else if let Some(n) = self.audio_notice() {
            status.push((n, warn));
        } else if !self.output_errors.is_empty() {
            status.push((
                "Some outputs are unavailable (Audio ▸ Outputs)".into(),
                warn,
            ));
        }
        if !snap.missing.is_empty() {
            status.push((format!("{} missing file(s)", snap.missing.len()), warn));
        }
        if self.midi.active_recently(150) {
            status.push(("● MIDI".into(), Color32::from_rgb(0xE5, 0x39, 0x35)));
        }
        let status_w = if status.is_empty() {
            0.0
        } else {
            (right - x) * 0.35
        };

        // Page tabs share the remaining width; they shrink rather than overflow.
        let n = snap.project.pages.len() + usize::from(self.edit_mode);
        let tabs_right = right - status_w - gap;
        let avail = (tabs_right - x).max(40.0);
        let tab_w = ((avail - gap * (n as f32 - 1.0)) / n as f32).clamp(24.0, btn_h * 3.2);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let released = ui.input(|i| i.pointer.any_released());
        let mut tab_rects = Vec::new();
        for (i, p) in snap.project.pages.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(x, y0), Vec2::new(tab_w, btn_h));
            x = r.right() + gap;
            tab_rects.push(r);
            // Inline rename.
            if let Some((ri, buf)) = &mut self.renaming_page
                && *ri == i
            {
                let resp = ui.put(
                    r,
                    egui::TextEdit::singleline(buf)
                        .font(egui::FontId::proportional(text))
                        .align(egui::Align2::CENTER_CENTER),
                );
                resp.request_focus();
                let (enter, esc) =
                    ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                if esc {
                    self.renaming_page = None;
                } else if enter || resp.lost_focus() {
                    let name = buf.trim().to_string();
                    if !name.is_empty() {
                        intents.push(Intent::RenamePage(i, name));
                    }
                    self.renaming_page = None;
                }
                continue;
            }
            let playing = p.cells.iter().any(|c| {
                snap.runtime
                    .get(&c.cart.id)
                    .is_some_and(|r| matches!(r.state, CartState::Playing | CartState::Stopping))
            });
            let label = if playing {
                format!("▶ {}", p.name)
            } else {
                p.name.clone()
            };
            let current = i == snap.page;
            let mut b =
                egui::Button::new(egui::RichText::new(label).size(text).color(if current {
                    Color32::WHITE
                } else {
                    ui.visuals().text_color()
                }))
                .truncate()
                .sense(Sense::click_and_drag());
            b = b.fill(if current {
                Color32::from_rgb(0x2F, 0x5F, 0x9A)
            } else {
                neutral
            });
            let resp = ui.put(r, b);
            if resp.double_clicked() {
                self.renaming_page = Some((i, p.name.clone()));
            } else if resp.clicked() {
                intents.push(Intent::SetPage(i));
            }
            if resp.drag_started() {
                self.page_drag = Some(i);
            }
            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    self.renaming_page = Some((i, p.name.clone()));
                    ui.close();
                }
                if ui.button("Page Settings…").clicked() {
                    self.dialogs.page_settings = Some(i);
                    crate::native::raise(ui.ctx(), "page-settings");
                    ui.close();
                }
                ui.separator();
                if ui.button("Copy Page").clicked() {
                    let clip = self.lock().copy_page(i);
                    self.page_clip = clip;
                    ui.close();
                }
                if let Some(clip) = self.page_clip.clone()
                    && ui.button("Paste Page (replaces this page)").clicked()
                {
                    intents.push(Intent::PastePage(i, clip));
                    ui.close();
                }
                ui.separator();
                if ui.button("Stop this page").clicked() {
                    intents.push(Intent::StopPage(i));
                    ui.close();
                }
                if ui.button("Add page").clicked() {
                    intents.push(Intent::AddPage);
                    ui.close();
                }
                if multi && ui.button("Remove page").clicked() {
                    intents.push(Intent::RemovePage(i));
                    ui.close();
                }
            });
        }
        // Dragging a tab: show where it will land; drop to move it there.
        if let Some(from) = self.page_drag {
            let to = pointer.map(|p| {
                tab_rects
                    .iter()
                    .position(|r| p.x < r.center().x)
                    .unwrap_or(tab_rects.len())
            });
            if let Some(to) = to {
                let lx = if to < tab_rects.len() {
                    tab_rects[to].left() - gap / 2.0
                } else {
                    tab_rects.last().map_or(x, |r| r.right() + gap / 2.0)
                };
                ui.painter().line_segment(
                    [Pos2::new(lx, y0 - 2.0), Pos2::new(lx, y0 + btn_h + 2.0)],
                    egui::Stroke::new(3.0, Color32::from_rgb(0x80, 0xC0, 0xFF)),
                );
            }
            if released {
                if let Some(to) = to {
                    let dest = if to > from { to - 1 } else { to };
                    if dest != from {
                        intents.push(Intent::MovePage(from, dest));
                    }
                }
                self.page_drag = None;
            }
        }
        if self.edit_mode {
            let r = Rect::from_min_size(Pos2::new(x, y0), Vec2::new(btn_h.min(tab_w), btn_h));
            if ui
                .put(
                    r,
                    egui::Button::new(egui::RichText::new("+").size(text)).fill(neutral),
                )
                .on_hover_text("Add page")
                .clicked()
            {
                intents.push(Intent::AddPage);
            }
        }

        if !status.is_empty() {
            let sr = Rect::from_min_max(
                Pos2::new(right - status_w, y0),
                Pos2::new(right, y0 + btn_h),
            );
            let mut job = egui::text::LayoutJob::default();
            for (i, (t, c)) in status.iter().enumerate() {
                if i > 0 {
                    job.append(
                        "  ·  ",
                        0.0,
                        egui::TextFormat::simple(
                            egui::FontId::proportional(12.0),
                            ui.visuals().weak_text_color(),
                        ),
                    );
                }
                job.append(
                    t,
                    0.0,
                    egui::TextFormat::simple(egui::FontId::proportional(12.0), *c),
                );
            }
            job.wrap = egui::text::TextWrapping::truncate_at_width(sr.width());
            job.halign = egui::Align::RIGHT;
            let galley = ui.painter().layout_job(job);
            ui.painter().with_clip_rect(sr).galley(
                Pos2::new(sr.right(), sr.center().y - galley.size().y / 2.0),
                galley,
                ui.visuals().text_color(),
            );
            let resp = ui.interact(sr, egui::Id::unique("status-line"), Sense::click());
            if !snap.missing.is_empty() && resp.clicked() {
                self.dialogs.relink = true;
            }
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let Some(page) = snap.project.pages.get(snap.page) else {
            return;
        };
        let avail = ui.available_rect_before_wrap();
        let gap = 6.0;
        let cw = ((avail.width() - gap * (page.cols as f32 - 1.0)) / page.cols as f32).max(40.0);
        let ch = ((avail.height() - gap * (page.rows as f32 - 1.0)) / page.rows as f32).max(30.0);
        let mut cells_geom = Vec::new();
        let (primary_down, primary_up, secondary_down, mods, pointer) = ui.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Primary),
                i.pointer.button_released(PointerButton::Primary),
                i.pointer.button_pressed(PointerButton::Secondary),
                i.modifiers,
                i.pointer.interact_pos(),
            )
        });
        // macOS: Control-click is a right-click.
        let ctrl_click = cfg!(target_os = "macos") && primary_down && mods.ctrl;
        let primary_down = primary_down && !ctrl_click;
        let secondary_down = secondary_down || ctrl_click;
        let hovered_files = ui.input(|i| !i.raw.hovered_files.is_empty());
        let shift = mods.shift;
        let popup_open = self.quick_edit.is_some();
        // Touch screens have no right button: in edit mode a tap selects (and plays) on
        // release, and press-and-hold opens quick edit instead of playing. Show mode
        // keeps play-on-press for the lowest latency.
        let touch_edit = self.edit_mode && ui.input(|i| i.any_touches());

        for row in 0..page.rows {
            for col in 0..page.cols {
                let min = avail.min + Vec2::new(col as f32 * (cw + gap), row as f32 * (ch + gap));
                let rect = Rect::from_min_size(min, Vec2::new(cw, ch));
                cells_geom.push((rect, row, col));
                let id = egui::Id::unique(("cell", snap.page, row, col));
                let resp = ui.interact(rect, id, Sense::click_and_drag());
                let cell = page.cell(row, col);
                let st = TileState {
                    selected: cell.is_some_and(|c| self.selected == Some(c.cart.id))
                        || (cell.is_none() && self.cursor_empty == Some((snap.page, row, col))),
                    learning: cell
                        .is_some_and(|c| snap.learning.is_some_and(|l| l.cart == c.cart.id)),
                    drop_hover: self.drag_hover.is_some_and(|p| rect.contains(p)),
                    edit_mode: self.edit_mode,
                    number: Some(row * page.cols + col + 1),
                    rerouted: cell.is_some_and(|c| self.is_rerouted(&c.cart.output)),
                };
                match cell {
                    Some(c) => crate::tile::paint_cart(
                        ui,
                        rect,
                        &c.cart,
                        &snap.project,
                        snap.runtime.get(&c.cart.id).copied().unwrap_or_default(),
                        &st,
                    ),
                    None => crate::tile::paint_empty(ui, rect, &st),
                }
                let hovered = pointer.is_some_and(|p| rect.contains(p)) && resp.hovered();
                if let Some(c) = cell
                    && let Some(t) = self.reroute_text(&c.cart.output)
                {
                    resp.clone().on_hover_text(t);
                }

                match cell {
                    Some(c) => {
                        let cart = c.cart.id;
                        // Trigger on press, not on release.
                        // A click that only dismisses the quick-edit popup does not play.
                        let tap = touch_edit && resp.clicked() && !resp.long_touched();
                        if (hovered && primary_down && !touch_edit || tap)
                            && !shift
                            && !mods.alt
                            && !popup_open
                        {
                            intents.push(Intent::PointerDown(cart, MouseButton::Primary));
                            self.pressed = Some(cart);
                            self.selected = Some(cart);
                            self.cursor_empty = None;
                        }
                        if (hovered && secondary_down) || (self.edit_mode && resp.long_touched()) {
                            if self.edit_mode {
                                self.selected = Some(cart);
                                self.quick_edit = Some((cart, pointer.unwrap_or(rect.center())));
                            } else {
                                intents.push(Intent::PointerDown(cart, MouseButton::Secondary));
                            }
                        }
                        // Shift-drag moves a cart; ⌥ Option-drag duplicates it.
                        if self.edit_mode && resp.drag_started() && (shift || mods.alt) {
                            self.dragging = Some((snap.page, row, col));
                            self.drag_copy = mods.alt && !shift;
                        }
                    }
                    None => {
                        if self.edit_mode {
                            let assign = Purpose::Assign {
                                page: snap.page,
                                row,
                                col,
                                many: true,
                            };
                            if resp.double_clicked() {
                                self.files.request(assign.clone());
                            }
                            resp.context_menu(|ui| {
                                if ui.button("Assign audio file(s)…").clicked() {
                                    self.files.request(assign.clone());
                                    ui.close();
                                }
                                if let Some(clip) = self.clipboard.clone()
                                    && ui.button("Paste cart").clicked()
                                {
                                    intents.push(Intent::Paste(snap.page, row, col, clip));
                                    ui.close();
                                }
                                ui.separator();
                                ui.menu_button("Control Button", |ui| {
                                    ui.menu_button("Stop Group", |ui| {
                                        for (i, g) in snap.project.groups.iter().enumerate() {
                                            if ui.button(g.label(i)).clicked() {
                                                intents.push(Intent::CreateControl(
                                                    snap.page,
                                                    row,
                                                    col,
                                                    bubble_project::Control::StopGroup,
                                                    Some(g.id),
                                                ));
                                                ui.close();
                                            }
                                        }
                                    });
                                    if ui.button("Stop This Page").clicked() {
                                        intents.push(Intent::CreateControl(
                                            snap.page,
                                            row,
                                            col,
                                            bubble_project::Control::StopPage,
                                            None,
                                        ));
                                        ui.close();
                                    }
                                    if ui.button("Stop All").clicked() {
                                        intents.push(Intent::CreateControl(
                                            snap.page,
                                            row,
                                            col,
                                            bubble_project::Control::StopAll,
                                            None,
                                        ));
                                        ui.close();
                                    }
                                });
                            });
                        }
                    }
                }
                if let Some(from) = self.dragging {
                    let over = pointer.is_some_and(|p| rect.contains(p));
                    if over && from != (snap.page, row, col) {
                        ui.painter().rect_stroke(
                            rect,
                            6.0,
                            egui::Stroke::new(3.0, Color32::from_rgb(0x80, 0xC0, 0xFF)),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if primary_up && over {
                        if self.drag_copy {
                            let src = page.cell(from.1, from.2).map(|c| c.cart.id);
                            if let Some(clip) = src.and_then(|id| self.lock().copy_cart(id)) {
                                intents.push(Intent::Paste(snap.page, row, col, clip));
                            }
                        } else {
                            intents.push(Intent::MoveCell(from, (snap.page, row, col)));
                        }
                        self.dragging = None;
                    }
                }
            }
        }
        if primary_up {
            if let Some(c) = self.pressed.take() {
                intents.push(Intent::PointerUp(c));
            }
            self.dragging = None;
        }
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::unique("cells"), cells_geom));

        // An empty project shows the mark and how to start (painted only: the cells
        // underneath still take clicks and drops).
        if snap.project.carts().next().is_none() && !hovered_files {
            let card = Rect::from_center_size(avail.center(), Vec2::new(460.0, 300.0))
                .intersect(avail.shrink(12.0));
            let painter = ui.painter_at(card.expand(24.0));
            painter.add(
                egui::epaint::Shadow {
                    offset: [0, 6],
                    blur: 24,
                    spread: 0,
                    color: Color32::from_black_alpha(90),
                }
                .as_shape(card, 16),
            );
            painter.rect_filled(card, 16.0, ui.visuals().window_fill);
            let lock = Rect::from_min_max(
                card.min + Vec2::new(0.0, 24.0),
                Pos2::new(card.max.x, card.min.y + 24.0 + card.height() * 0.6),
            );
            crate::logo::paint_lockup(&painter, lock, ui.visuals().strong_text_color());
            let hint = ui.visuals().weak_text_color();
            let mut y = lock.bottom() + 18.0;
            if snap.path.is_none() {
                // No session yet: start one or open one.
                let bw = 150.0;
                let r1 = Rect::from_min_size(
                    Pos2::new(card.center().x - bw - 6.0, y),
                    Vec2::new(bw, 34.0),
                );
                let r2 =
                    Rect::from_min_size(Pos2::new(card.center().x + 6.0, y), Vec2::new(bw, 34.0));
                if ui
                    .put(
                        r1,
                        egui::Button::new(egui::RichText::new("New Session…").strong())
                            .fill(Color32::from_rgb(0x2F, 0x6B, 0xFF)),
                    )
                    .clicked()
                {
                    self.request(PendingAction::New, snap, intents);
                }
                if ui.put(r2, egui::Button::new("Open…")).clicked() {
                    self.files.request(Purpose::Open);
                }
                return;
            }
            for line in [
                "Drop audio files onto the grid, or double-click a cell.",
                "Right-click a cart for quick edits.",
                "Audio ▸ Outputs chooses where carts play.",
            ] {
                painter.text(
                    Pos2::new(card.center().x, y),
                    egui::Align2::CENTER_TOP,
                    line,
                    egui::FontId::proportional(14.0),
                    hint,
                );
                y += 20.0;
            }
        }
    }

    /// Right-click (or Control-click on macOS) in edit mode: the common cart settings
    /// in a popup that stays open until you click elsewhere or press Esc.
    fn quick_edit_popup(
        &mut self,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        let Some((id, pos)) = self.quick_edit else {
            return;
        };
        let Some(cart) = snap.project.cart(id).cloned() else {
            self.quick_edit = None;
            return;
        };
        if !self.edit_mode {
            self.quick_edit = None;
            return;
        }
        let mut close = false;
        let mut c = cart.clone();
        let control = cart.control;
        let out_labels: Vec<String> = {
            let n = self
                .prefs
                .effective_outputs()
                .len()
                .max(bubble_project::output_index(&cart.output).map_or(0, |i| i + 1));
            (0..n).map(|i| self.output_label(i, false)).collect()
        };
        let trigger_text = snap
            .project
            .bindings
            .iter()
            .filter(|b| b.targets.contains(&id))
            .map(|b| describe(&b.source))
            .collect::<Vec<_>>()
            .join(", ");
        let mut picker_open = self.qe_picker;
        let button = snap
            .project
            .cart_position(id)
            .and_then(|(p, r, c)| snap.project.pages.get(p).map(|pg| r * pg.cols + c + 1))
            .unwrap_or(0);
        const W: f32 = 340.0;
        const LABEL_W: f32 = 64.0;
        let area = egui::Area::new(egui::Id::unique("quick-edit"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(W);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(match control {
                                    Some(k) => format!("{} - Button {button}", k.name()),
                                    None => format!("Quick edit - Button {button}"),
                                })
                                .strong(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Done").clicked() {
                                        close = true;
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                        let row =
                            |ui: &mut egui::Ui, label: &str| field_label_w(ui, label, LABEL_W);
                        egui::Grid::new("qe-grid")
                            .num_columns(2)
                            .spacing([8.0, 8.0])
                            .show(ui, |ui| {
                                row(ui, "Label");
                                ui.add(
                                    egui::TextEdit::singleline(&mut c.label)
                                        .hint_text(display_label(&cart, &snap.project))
                                        .desired_width(W - LABEL_W - 8.0),
                                );
                                ui.end_row();
                                row(ui, "Colour");
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    colour_swatches(ui, &mut c.color, &mut picker_open);
                                });
                                ui.end_row();
                                if control.is_none() {
                                    row(ui, "Gain");
                                    ui.add(
                                        egui::Slider::new(&mut c.gain_db, -12.0..=12.0)
                                            .clamping(egui::SliderClamping::Edits)
                                            .suffix(" dB")
                                            .fixed_decimals(1),
                                    );
                                    ui.end_row();
                                    row(ui, "Speed");
                                    ui.add(
                                        egui::Slider::new(&mut c.speed, 0.25..=4.0)
                                            .logarithmic(true)
                                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                            .custom_parser(parse_percent),
                                    );
                                    ui.end_row();
                                    row(ui, "Output");
                                    output_combo(ui, "qe-output", &out_labels, &mut c.output);
                                    ui.end_row();
                                    row(ui, "Fade out");
                                    fade_picker_ex(
                                        ui,
                                        "qe-fade-out",
                                        &mut c.fade_out,
                                        Some(snap.project.settings.default_fade_out_ms),
                                        false,
                                    );
                                    ui.end_row();
                                }
                                if control.is_none()
                                    || control == Some(bubble_project::Control::StopGroup)
                                {
                                    row(ui, "Group");
                                    group_combo(ui, "qe-group", &snap.project, &mut c.group);
                                    ui.end_row();
                                }
                                row(ui, "Trigger");
                                ui.horizontal(|ui| {
                                    ui.label(if trigger_text.is_empty() {
                                        egui::RichText::new("none").weak()
                                    } else {
                                        egui::RichText::new(&trigger_text).monospace()
                                    });
                                    if ui.small_button("Learn").clicked() {
                                        intents.push(Intent::LearnDefault(id));
                                        close = true;
                                    }
                                });
                                ui.end_row();
                                if control.is_none() {
                                    row(ui, "Options");
                                    egui::Grid::new("qe-opts")
                                        .num_columns(3)
                                        .spacing([10.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.checkbox(&mut c.looping, "Loop");
                                            ui.checkbox(&mut c.hold_to_play, "Hold");
                                            ui.checkbox(&mut c.muted, "Mute");
                                            ui.end_row();
                                            let mut next = c.play_next.is_some();
                                            if ui.checkbox(&mut next, "Play next").changed() {
                                                c.play_next = next.then(|| "next_cell".to_string());
                                            }
                                            let mut stop = c.on_play.stop != StopScope::None;
                                            if ui.checkbox(&mut stop, "Stop others").changed() {
                                                c.on_play.stop = if stop {
                                                    StopScope::All
                                                } else {
                                                    StopScope::None
                                                };
                                            }
                                            ui.end_row();
                                        });
                                    ui.end_row();
                                }
                            });
                        ui.add_space(4.0);
                        ui.separator();
                        // Actions: equal-width buttons in rows of three.
                        let mut actions: Vec<(&str, u8)> = Vec::new();
                        if control.is_none() {
                            actions.extend([
                                ("Cart Setup…", 1),
                                ("Speed Bar…", 2),
                                ("Replace file…", 3),
                            ]);
                        }
                        actions.push(("Copy", 4));
                        if self.clipboard.is_some() {
                            actions.push(("Paste over", 5));
                        }
                        actions.push(("Clear", 6));
                        let bw = (W - 2.0 * 6.0) / 3.0;
                        let mut chosen = None;
                        for chunk in actions.chunks(3) {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                for (label, k) in chunk {
                                    if ui
                                        .add_sized([bw, 26.0], egui::Button::new(*label))
                                        .clicked()
                                    {
                                        chosen = Some(*k);
                                    }
                                }
                            });
                        }
                        let at = snap.project.cart_position(id);
                        match chosen {
                            Some(1) => {
                                self.open_audio_setup(id);
                                close = true;
                            }
                            Some(2) => {
                                self.open_speed_bar(id);
                                close = true;
                            }
                            Some(3) => {
                                if let Some((page, row, col)) = at {
                                    self.files.request(Purpose::Assign {
                                        page,
                                        row,
                                        col,
                                        many: false,
                                    });
                                }
                                close = true;
                            }
                            Some(4) => {
                                let clip = self.lock().copy_cart(id);
                                self.clipboard = clip;
                                close = true;
                            }
                            Some(5) => {
                                if let (Some(clip), Some((p, r, col))) =
                                    (self.clipboard.clone(), at)
                                {
                                    intents.push(Intent::Paste(p, r, col, clip));
                                }
                                close = true;
                            }
                            Some(6) => {
                                if let Some((p, r, col)) = at {
                                    intents.push(Intent::ClearCell(p, r, col));
                                    self.selected = None;
                                }
                                close = true;
                            }
                            _ => {}
                        }
                    });
            });
        self.qe_picker = picker_open;
        if c != cart {
            intents.push(Intent::ReplaceCart(c));
        }
        // A click outside closes it, except one in a drop-down list that belongs to it
        // (those lists are separate layers outside the popup's rectangle).
        let (pressed_outside, esc) = ctx.input(|i| {
            let outside = i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p));
            (outside, i.key_pressed(Key::Escape))
        });
        let list_open = egui::Popup::is_any_open(ctx);
        let just_opened = ctx.input(|i| i.pointer.button_pressed(PointerButton::Secondary))
            || ctx.input(|i| i.pointer.button_pressed(PointerButton::Primary) && i.modifiers.ctrl);
        if close || (esc && !list_open) || (pressed_outside && !just_opened && !list_open) {
            self.quick_edit = None;
            self.qe_picker = false;
        }
    }

    fn triggers_ui(&self, ui: &mut egui::Ui, snap: &Snapshot, id: Uuid, intents: &mut Vec<Intent>) {
        for b in snap
            .project
            .bindings
            .iter()
            .filter(|b| b.targets.contains(&id))
        {
            let dups = snap
                .project
                .bindings
                .iter()
                .filter(|o| o.source == b.source && o.id != b.id)
                .count();
            ui.horizontal(|ui| {
                ui.monospace(describe(&b.source));
                let mut a = b.action;
                egui::ComboBox::from_id_salt(("binding-action", b.id))
                    .selected_text(action_name(a))
                    .show_ui(ui, |ui| {
                        for x in [Action::Toggle, Action::Play, Action::Stop] {
                            ui.selectable_value(&mut a, x, action_name(x));
                        }
                    });
                if a != b.action {
                    intents.push(Intent::SetBindingAction(b.id, a));
                }
                if dups > 0 {
                    ui.label(format!("(shared with {dups})"));
                }
            });
        }
        ui.horizontal(|ui| {
            if snap.learning.is_some_and(|l| l.cart == id) {
                ui.colored_label(
                    Color32::from_rgb(0xFF, 0x60, 0xFF),
                    "Press a key or send MIDI… (Esc cancels)",
                );
                if ui.button("Cancel").clicked() {
                    intents.push(Intent::CancelLearn);
                }
            } else {
                if ui
                    .button("Learn")
                    .on_hover_text(
                        "The next key or MIDI message plays this cart; pressing it again stops it.",
                    )
                    .clicked()
                {
                    intents.push(Intent::LearnDefault(id));
                }
                if ui.button("Clear key").clicked() {
                    intents.push(Intent::ClearBindings(id, Some(true)));
                }
                if ui.button("Clear MIDI").clicked() {
                    intents.push(Intent::ClearBindings(id, Some(false)));
                }
            }
        });
    }

    fn windows(&mut self, ctx: &egui::Context, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if self.dialogs.diagnostics {
            let (open, f) = native_window(
                ctx,
                WindowSpec {
                    key: "diagnostics",
                    title: "Audio Diagnostics",
                    size: [520.0, 300.0],
                    min_size: [360.0, 200.0],
                    on_top: false,
                    fit: true,
                },
                |ui| self.diagnostics_ui(ui),
            );
            self.dialogs.diagnostics = open;
            self.child_focus |= f;
        }

        self.new_session_modal(ctx, intents);
        self.import_choice_modal(ctx, snap, intents);
        self.convert_offer_modal(ctx);
        self.audio_setup_window(ctx, snap, intents);
        self.project_settings_window(ctx, snap, intents);
        self.outputs_window(ctx);
        self.reassign_window(ctx, snap, intents);
        self.page_settings_window(ctx, snap, intents);
        self.speed_bar_windows(ctx, snap, intents);

        if self.dialogs.relink {
            let (open, f) = native_window(
                ctx,
                WindowSpec {
                    key: "relink",
                    title: "Missing Files",
                    size: [560.0, 300.0],
                    min_size: [320.0, 160.0],
                    on_top: false,
                    fit: true,
                },
                |ui| {
                    if snap.missing.is_empty() {
                        ui.label("All audio files were found.");
                    }
                    for a in &snap.missing {
                        let path = snap
                            .project
                            .asset(*a)
                            .map(|x| x.path.clone())
                            .unwrap_or_default();
                        ui.horizontal(|ui| {
                            ui.label(&path);
                            if ui.button("Locate…").clicked() {
                                self.files.request(Purpose::Relink(*a));
                            }
                        });
                    }
                    ui.label(
                        "Locating one file also relinks other missing files that moved together.",
                    );
                },
            );
            self.dialogs.relink = open;
            self.child_focus |= f;
        }

        if self.dialogs.log {
            let (open, f) = native_window(
                ctx,
                WindowSpec {
                    key: "log",
                    title: "Playout Log",
                    size: [560.0, 420.0],
                    min_size: [320.0, 200.0],
                    on_top: false,
                    fit: false,
                },
                |ui| self.log_ui(ui),
            );
            self.dialogs.log = open;
            self.child_focus |= f;
        }

        if !self.dialogs.recover.is_empty() {
            let mut done: Option<usize> = None;
            egui::Modal::new(egui::Id::unique("recover")).show(ctx, |ui| {
                ui.heading("Recover Unsaved Work");
                ui.label("Bubble found unsaved changes from a previous session:");
                let items = self.dialogs.recover.clone();
                for (i, (file, project)) in items.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let name = project
                            .as_ref()
                            .unwrap_or(file)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        ui.label(name);
                        if ui.button("Restore").clicked() {
                            match bubble_project::io::load(file.as_path()) {
                                Ok(p) => {
                                    let mut c = self.lock();
                                    c.replace_project(p, project.clone());
                                    c.set_dirty();
                                }
                                Err(e) => {
                                    self.dialogs.error = Some(format!("Could not restore: {e}"))
                                }
                            }
                            done = Some(i);
                        }
                        if ui.button("Discard").clicked() {
                            let _ = std::fs::remove_file(file);
                            done = Some(i);
                        }
                    });
                }
                ui.label(
                    egui::RichText::new(
                        "Restoring does not overwrite the project file until you save.",
                    )
                    .small()
                    .weak(),
                );
            });
            if let Some(i) = done {
                let (file, project) = self.dialogs.recover.remove(i);
                // A restored untitled recovery file is no longer needed once loaded.
                if project.is_none() {
                    let _ = std::fs::remove_file(file);
                }
            }
        }

        if self.dialogs.paste_attrs {
            let mut apply = false;
            let mut cancel = false;
            let resp = egui::Modal::new(egui::Id::unique("paste-attrs")).show(ctx, |ui| {
                ui.heading("Paste Attributes");
                let from = self
                    .clipboard
                    .as_ref()
                    .map(|c| display_label(&c.cart, &snap.project))
                    .unwrap_or_default();
                ui.label(format!(
                    "Copy from \u{201c}{from}\u{201d} to the selected cart:"
                ));
                let a = &mut self.attr_choice;
                ui.checkbox(&mut a.gain, "Gain and mute");
                ui.checkbox(&mut a.pan, "Pan");
                ui.checkbox(&mut a.fades, "Fades");
                ui.checkbox(&mut a.colour, "Colour");
                ui.checkbox(&mut a.looping, "Loop");
                ui.checkbox(
                    &mut a.behaviour,
                    "Retrigger, stop-others, group, hold-to-play",
                );
                crate::native::dialog_bar(ui, |ui| {
                    apply = ui.add(crate::native::primary("Paste")).clicked();
                    cancel = ui.add(crate::native::secondary("Cancel")).clicked();
                });
            });
            if apply && let (Some(clip), Some(sel)) = (self.clipboard.clone(), self.selected) {
                intents.push(Intent::PasteAttrs(clip.cart, vec![sel], self.attr_choice));
            }
            if apply || cancel || resp.should_close() {
                self.dialogs.paste_attrs = false;
            }
        }

        if let Some(err) = self.dialogs.error.clone() {
            let mut ok = false;
            let resp = egui::Modal::new(egui::Id::unique("error")).show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.heading("Problem");
                // Selectable and copyable, so the exact message can be passed on.
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(&err).selectable(true).wrap());
                    });
                crate::native::dialog_bar(ui, |ui| {
                    ok = ui.add(crate::native::primary("OK")).clicked();
                    if ui.add(crate::native::secondary("Copy")).clicked() {
                        ui.ctx().copy_text(err.clone());
                    }
                });
            });
            if ok || resp.should_close() {
                self.dialogs.error = None;
            }
        }

        if let Some(action) = self.dialogs.confirm_discard.clone() {
            let resp = egui::Modal::new(egui::Id::unique("confirm-discard")).show(ctx, |ui| {
                ui.heading("Unsaved changes");
                ui.label("The current project has unsaved changes.");
                crate::native::dialog_bar(ui, |ui| {
                    if ui.add(crate::native::primary("Save first")).clicked() {
                        self.save_or_save_as(snap, intents);
                        self.perform(action.clone(), intents);
                        self.dialogs.confirm_discard = None;
                    }
                    if ui.add(crate::native::secondary("Cancel")).clicked() {
                        self.dialogs.confirm_discard = None;
                    }
                    if ui
                        .add(crate::native::secondary("Discard changes"))
                        .clicked()
                    {
                        self.perform(action.clone(), intents);
                        self.dialogs.confirm_discard = None;
                    }
                });
            });
            if resp.should_close() {
                self.dialogs.confirm_discard = None;
            }
        }
    }

    fn diagnostics_ui(&mut self, ui: &mut egui::Ui) {
        for e in &self.output_errors {
            ui.colored_label(Color32::from_rgb(0xFF, 0x90, 0x40), e);
        }
        for a in self.audio.iter().skip(1) {
            ui.label(format!(
                "Also: {} — {} Hz, {} ch, {}",
                a.device_name, a.sample_rate, a.channels, a.sample_format
            ));
        }
        match self.audio.first() {
            Some(a) => {
                let d = self
                    .lock()
                    .engine()
                    .map(|e| e.status().diagnostics())
                    .unwrap_or_default();
                ui.label(format!(
                    "{} — {} Hz, {} ch, {} samples, requested buffer {:?}\n\
                     callback frames min/last/max {}/{}/{}\n\
                     reported output latency {:.2} ms\n\
                     max callback processing {:.3} ms\n\
                     active voices {} · xruns {} · voices stolen {} · events dropped {}\n\
                     disk underruns {} ({} frames silent) · waits after a jump {} frames",
                    a.device_name,
                    a.sample_rate,
                    a.channels,
                    a.sample_format,
                    a.buffer,
                    d.min_callback_frames,
                    d.last_callback_frames,
                    d.max_callback_frames,
                    d.output_latency_us as f64 / 1000.0,
                    d.max_process_ns as f64 / 1e6,
                    d.active_voices,
                    d.xruns,
                    d.voices_stolen,
                    d.events_dropped,
                    d.stream_underruns,
                    d.stream_underrun_frames,
                    d.stream_wait_frames
                ));
            }
            None => {
                ui.colored_label(
                    Color32::from_rgb(0xFF, 0x60, 0x60),
                    self.audio_error
                        .clone()
                        .unwrap_or_else(|| "No audio output".into()),
                );
            }
        }
        let (used, limit) = self.lock().audio_memory();
        ui.label(format!(
            "Audio memory {:.2} GB of {:.1} GB",
            used as f64 / 1e9,
            limit as f64 / 1e9
        ));
        if ui.button("Reopen audio output").clicked() {
            self.open_audio();
        }
        ui.separator();
        ui.label(format!(
            "MIDI ports open: {}",
            self.midi.open_ports().join(", ")
        ));
        ui.label(format!(
            "Max controller-lock wait in MIDI callback: {} µs",
            self.midi
                .max_lock_wait_us
                .load(std::sync::atomic::Ordering::Relaxed)
        ));
        for e in &self.midi.errors {
            ui.colored_label(Color32::from_rgb(0xFF, 0x60, 0x60), e);
        }
    }

    fn log_ui(&self, ui: &mut egui::Ui) {
        let c = self.lock();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for e in c
                    .log()
                    .iter()
                    .rev()
                    .take(500)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    let t = e
                        .time
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() % 86_400)
                        .unwrap_or(0);
                    let ts = format!("{:02}:{:02}:{:02} UTC", t / 3600, (t / 60) % 60, t % 60);
                    let what = match &e.kind {
                        LogKind::Started(s) => format!("started ({s:?})"),
                        LogKind::Ended(r) => format!("ended ({r:?})"),
                        LogKind::Ignored => "ignored".into(),
                        LogKind::Rejected(r) => format!("REJECTED: {r}"),
                        LogKind::Info(m) => m.clone(),
                    };
                    ui.monospace(format!("{ts}  {:<28} {what}", e.label));
                }
            });
    }

    /// Session ▸ Session Settings: name, output level, fades, tile appearance, groups.
    fn project_settings_window(
        &mut self,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        if !self.dialogs.project_settings {
            return;
        }
        let font_names = self.font_names.clone();
        let (open, f) = native_window(
            ctx,
            WindowSpec {
                key: "project-settings",
                title: "Session Settings",
                size: [520.0, 640.0],
                min_size: [420.0, 360.0],
                on_top: false,
                fit: false,
            },
            |ui| {
                self.keyboard(ui.ctx(), snap, intents);
                let mut st = snap.project.settings.clone();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Project");
                    let mut name = snap.project.name.clone();
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        if ui.text_edit_singleline(&mut name).changed() {
                            intents.push(Intent::RenameProject(name));
                        }
                    });
                    egui::Grid::new("proj-grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Output level");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Slider::new(&mut st.master_gain_db, -40.0..=0.0)
                                        .suffix(" dB")
                                        .step_by(0.5),
                                );
                                if (st.master_gain_db - bubble_project::DEFAULT_MASTER_GAIN_DB)
                                    .abs()
                                    > 0.01
                                    && ui.small_button("−10 dB").clicked()
                                {
                                    st.master_gain_db = bubble_project::DEFAULT_MASTER_GAIN_DB;
                                }
                            });
                            ui.end_row();
                            ui.label("Default fade-out");
                            {
                                let mut f = Some(FadeSpec::ms(st.default_fade_out_ms));
                                fade_picker(ui, "default-fade-out", &mut f, None);
                                st.default_fade_out_ms = f.map_or(0, |f| f.ms).min(10_000);
                            }
                            ui.end_row();
                            ui.label("Ending warning");
                            ui.add(
                                egui::DragValue::new(&mut st.ending_warning_s)
                                    .range(0.0..=60.0)
                                    .speed(0.1)
                                    .suffix(" s"),
                            );
                            ui.end_row();
                            ui.label("Audio files");
                            egui::ComboBox::from_id_salt("audio-files")
                                .selected_text(if st.convert_to_wav {
                                    "Convert to WAV"
                                } else {
                                    "Keep as imported"
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut st.convert_to_wav,
                                        false,
                                        "Keep as imported",
                                    );
                                    ui.selectable_value(
                                        &mut st.convert_to_wav,
                                        true,
                                        "Convert to WAV",
                                    );
                                })
                                .response
                                .on_hover_text(
                                    "Convert to WAV converts each file once, when it is added, \
                                     so the session opens faster on slower computers. \
                                     Playback is the same either way.",
                                );
                            ui.end_row();
                        });
                    ui.label(
                        egui::RichText::new(
                            "Output level attenuates everything Bubble plays. −10 dB (the \
                             default) leaves headroom to raise individual carts.",
                        )
                        .small()
                        .weak(),
                    );

                    ui.separator();
                    ui.heading("Cart tiles");
                    let d = &mut st.display;
                    egui::Grid::new("display-grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Font");
                            egui::ComboBox::from_id_salt("font")
                                .selected_text(d.font.clone().unwrap_or_else(|| "Default".into()))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut d.font, None, "Default");
                                    for n in &font_names {
                                        ui.selectable_value(&mut d.font, Some(n.to_string()), *n);
                                    }
                                });
                            ui.end_row();
                            ui.label("Label size");
                            ui.add(
                                egui::Slider::new(&mut d.label_size, 0.5..=3.0)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                    .custom_parser(parse_percent),
                            );
                            ui.end_row();
                            ui.label("Small text size");
                            ui.add(
                                egui::Slider::new(&mut d.small_size, 0.5..=3.0)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                    .custom_parser(parse_percent),
                            )
                            .on_hover_text("Number, group, trigger, time and icons");
                            ui.end_row();
                            ui.label("Horizontal");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut d.h_align, HAlign::Left, "Left");
                                ui.selectable_value(&mut d.h_align, HAlign::Center, "Centre");
                                ui.selectable_value(&mut d.h_align, HAlign::Right, "Right");
                            });
                            ui.end_row();
                            ui.label("Vertical");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut d.v_align, VAlign::Top, "Top");
                                ui.selectable_value(&mut d.v_align, VAlign::Center, "Centre");
                                ui.selectable_value(&mut d.v_align, VAlign::Bottom, "Bottom");
                            });
                            ui.end_row();
                        });

                    ui.separator();
                    ui.heading("Exclusive groups");
                    ui.label(
                        egui::RichText::new(
                            "Starting a cart stops the other carts in its group. Named groups \
                             show as \u{201c}G1 Name\u{201d} on the tile.",
                        )
                        .small()
                        .weak(),
                    );
                    let mut groups = snap.project.groups.clone();
                    let mut remove: Option<usize> = None;
                    egui::Grid::new("groups-grid")
                        .num_columns(4)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            for (i, g) in groups.iter_mut().enumerate() {
                                ui.label(format!("G{}", i + 1));
                                ui.add_sized(
                                    [220.0, 20.0],
                                    egui::TextEdit::singleline(&mut g.name).hint_text("unnamed"),
                                );
                                let used = snap
                                    .project
                                    .carts()
                                    .filter(|(_, c)| c.cart.group == Some(g.id))
                                    .count();
                                ui.label(
                                    egui::RichText::new(format!("{used} cart(s)"))
                                        .small()
                                        .weak(),
                                );
                                if ui
                                    .small_button("Remove")
                                    .on_hover_text("Carts in this group become ungrouped")
                                    .clicked()
                                {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                    if let Some(i) = remove {
                        groups.remove(i);
                    }
                    if groups != snap.project.groups {
                        intents.push(Intent::SetGroups(groups));
                    }
                    if ui.button("Add group").clicked() {
                        intents.push(Intent::AddGroup);
                    }
                });
                if st != snap.project.settings {
                    intents.push(Intent::Settings(st));
                }
            },
        );
        self.dialogs.project_settings = open;
        self.child_focus |= f;
    }

    /// Page settings (Edit ▸ Page Settings, or right-click a page tab).
    fn page_settings_window(
        &mut self,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        let Some(pi) = self.dialogs.page_settings else {
            return;
        };
        let Some(page) = snap.project.pages.get(pi) else {
            self.dialogs.page_settings = None;
            return;
        };
        let (open, f) = native_window(
            ctx,
            WindowSpec {
                key: "page-settings",
                title: "Page Settings",
                size: [380.0, 220.0],
                min_size: [320.0, 180.0],
                on_top: false,
                fit: true,
            },
            |ui| {
                self.keyboard(ui.ctx(), snap, intents);
                ui.horizontal(|ui| {
                    ui.label("Page");
                    egui::ComboBox::from_id_salt("page-pick")
                        .selected_text(&page.name)
                        .show_ui(ui, |ui| {
                            for (i, p) in snap.project.pages.iter().enumerate() {
                                if ui.selectable_label(i == pi, &p.name).clicked() {
                                    self.dialogs.page_settings = Some(i);
                                }
                            }
                        });
                });
                ui.separator();
                egui::Grid::new("page-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        let mut pname = page.name.clone();
                        if ui.text_edit_singleline(&mut pname).changed() {
                            intents.push(Intent::RenamePage(pi, pname));
                        }
                        ui.end_row();
                        let (mut r, mut c) = (page.rows, page.cols);
                        ui.label("Rows × columns");
                        ui.horizontal(|ui| {
                            let a = ui.add(egui::DragValue::new(&mut r).range(1..=16));
                            ui.label("×");
                            let b = ui.add(egui::DragValue::new(&mut c).range(1..=16));
                            if a.changed() || b.changed() {
                                intents.push(Intent::ResizePage(pi, r, c));
                            }
                        });
                        ui.end_row();
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Add page").clicked() {
                        intents.push(Intent::AddPage);
                    }
                    if snap.project.pages.len() > 1 && ui.button("Remove this page").clicked() {
                        intents.push(Intent::RemovePage(pi));
                        self.dialogs.page_settings = None;
                    }
                });
            },
        );
        if !open {
            self.dialogs.page_settings = None;
        }
        self.child_focus |= f;
    }

    /// Floating speed bars, after the reference's: a small always-on-top window per
    /// cart (up to four) with `x1` (yellow when the speed is not normal; resets),
    /// a horizontal speed slider (25–400 %), and `x0` (blue while the cart plays, red
    /// while slowing to a stop; right-click it to choose the slow-down time). The
    /// actual speed is shown in the title bar. While the cart is not playing, the
    /// slider sets the cart's preset speed.
    fn speed_bar_windows(
        &mut self,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        let ids = self.dialogs.speed_bars.clone();
        let mut closed = Vec::new();
        for id in ids {
            let Some(cart) = snap.project.cart(id).cloned() else {
                closed.push(id);
                continue;
            };
            let rt = snap.runtime.get(&id).copied().unwrap_or_default();
            let playing = matches!(rt.state, CartState::Playing | CartState::Stopping);
            let live = if playing { rt.speed } else { cart.speed };
            let number = snap
                .project
                .cart_position(id)
                .and_then(|(p, r, c)| snap.project.pages.get(p).map(|pg| r * pg.cols + c + 1));
            let title = format!(
                "Btn{}  {}  {:.0}%",
                number.unwrap_or(0),
                display_label(&cart, &snap.project),
                live * 100.0
            );
            let key = format!("speed-{id}");
            let slowing = rt.slowed;
            let (open, f) = native_window(
                ctx,
                WindowSpec {
                    key: &key,
                    title: &title,
                    size: [440.0, 44.0],
                    min_size: [220.0, 36.0],
                    on_top: true,
                    fit: false,
                },
                |ui| {
                    self.keyboard(ui.ctx(), snap, intents);
                    let h = (ui.available_height()).clamp(24.0, 80.0);
                    // Right-clicking x0/x1 turns the bar into a row of times (a popup
                    // menu would be clipped by this small window).
                    if let Some(x0_pick) = self.speed_pick.get(&id).copied() {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(if x0_pick {
                                "x0 slow-down"
                            } else {
                                "x1 speed-up"
                            });
                            let current = if x0_pick {
                                cart.slowdown_ms
                            } else {
                                cart.speedup_ms
                            };
                            let choices: &[u32] = if x0_pick {
                                &[500, 1000, 2000, 3000, 4000, 5000, 8000]
                            } else {
                                &[0, 500, 1000, 2000, 3000, 5000]
                            };
                            for &ms in choices {
                                let label = if ms == 0 {
                                    "Instant".to_string()
                                } else {
                                    format!("{}s", ms as f32 / 1000.0)
                                };
                                if ui
                                    .add(
                                        egui::Button::selectable(current == ms, label)
                                            .min_size(Vec2::new(0.0, h - 8.0)),
                                    )
                                    .clicked()
                                {
                                    let mut c = cart.clone();
                                    if x0_pick {
                                        c.slowdown_ms = ms;
                                    } else {
                                        c.speedup_ms = ms;
                                    }
                                    intents.push(Intent::ReplaceCart(c));
                                    self.speed_pick.remove(&id);
                                }
                            }
                            if ui
                                .add(egui::Button::new("Cancel").min_size(Vec2::new(0.0, h - 8.0)))
                                .clicked()
                            {
                                self.speed_pick.remove(&id);
                            }
                        });
                        return;
                    }
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let off_normal = (live - 1.0).abs() > 0.005;
                        let mut x1 = egui::Button::new(egui::RichText::new("x1").strong().color(
                            if off_normal {
                                Color32::BLACK
                            } else {
                                ui.visuals().text_color()
                            },
                        ))
                        .min_size(Vec2::new(36.0, h));
                        if off_normal {
                            x1 = x1.fill(Color32::from_rgb(0xF5, 0xD0, 0x20));
                        }
                        let x1 = ui.add(x1).on_hover_text(if cart.speedup_ms == 0 {
                            "Back to normal speed (right-click to set the speed-up time)"
                                .to_string()
                        } else {
                            format!(
                                "Back to normal speed over {:.1} s (right-click to change)",
                                cart.speedup_ms as f32 / 1000.0
                            )
                        });
                        if x1.clicked() {
                            if playing {
                                intents.push(Intent::SpeedNormal(id));
                            } else {
                                let mut c = cart.clone();
                                c.speed = 1.0;
                                intents.push(Intent::ReplaceCart(c));
                            }
                        }
                        if x1.secondary_clicked() {
                            self.speed_pick.insert(id, false);
                        }
                        let x0_w = 36.0;
                        let arrow_w = 22.0;
                        let slider_w =
                            (ui.available_width() - x0_w - 2.0 * arrow_w - 4.0 * 6.0).max(60.0);
                        let set_speed = |v: f32, intents: &mut Vec<Intent>| {
                            if playing {
                                intents.push(Intent::LiveSpeed(id, v));
                            } else {
                                let mut c = cart.clone();
                                c.speed = (v * 100.0).round() / 100.0;
                                intents.push(Intent::ReplaceCart(c));
                            }
                        };
                        // Arrows step 1 % (fine control, as on the reference's scrollbar).
                        if ui
                            .add(egui::Button::new("◀").min_size(Vec2::new(arrow_w, h)))
                            .clicked()
                        {
                            set_speed((live - 0.01).max(0.25), intents);
                        }
                        ui.spacing_mut().slider_width = slider_w;
                        ui.spacing_mut().interact_size.y = h;
                        let mut v = live.max(0.25);
                        let r = ui.add(
                            egui::Slider::new(&mut v, 0.25..=4.0)
                                .logarithmic(true)
                                .show_value(false)
                                .trailing_fill(true),
                        );
                        if r.changed() {
                            set_speed(v, intents);
                        }
                        if ui
                            .add(egui::Button::new("▶").min_size(Vec2::new(arrow_w, h)))
                            .clicked()
                        {
                            set_speed((live + 0.01).min(4.0), intents);
                        }
                        let (fill, fg) = if slowing {
                            (Color32::from_rgb(0xC6, 0x28, 0x28), Color32::WHITE)
                        } else if playing {
                            (Color32::from_rgb(0x1E, 0x6F, 0xD9), Color32::WHITE)
                        } else {
                            (
                                ui.visuals().widgets.inactive.weak_bg_fill,
                                ui.visuals().weak_text_color(),
                            )
                        };
                        // Always enabled so right-click works; a click only acts while playing.
                        let x0 = ui
                            .add(
                                egui::Button::new(egui::RichText::new("x0").strong().color(fg))
                                    .fill(fill)
                                    .min_size(Vec2::new(x0_w, h)),
                            )
                            .on_hover_text(format!(
                                "Slow to a stop over {:.1} s (right-click to change)",
                                cart.slowdown_ms as f32 / 1000.0
                            ));
                        if x0.clicked() && playing {
                            intents.push(Intent::SlowToStop(id));
                        }
                        if x0.secondary_clicked() {
                            self.speed_pick.insert(id, true);
                        }
                    });
                },
            );
            if !open {
                closed.push(id);
            }
            self.child_focus |= f;
        }
        self.dialogs.speed_bars.retain(|x| !closed.contains(x));
    }

    fn audio_setup_window(
        &mut self,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        let Some(id) = self.dialogs.audio_setup else {
            return;
        };
        let Some(cart) = snap
            .project
            .cart(id)
            .cloned()
            .filter(|c| c.control.is_none())
        else {
            self.dialogs.audio_setup = None;
            self.setup_draft = None;
            return;
        };
        // Edits go to a draft; OK keeps them, Cancel throws them away.
        let mut c = match self.setup_draft.take() {
            Some(d) if d.id == id => d,
            _ => cart.clone(),
        };
        // The overview is read from the file in the background the first time.
        let ready = cart.asset.and_then(|a| {
            let mut ctl = self.lock();
            let source = match ctl.asset_state(a) {
                Some(AssetState::Ready { source, .. }) => *source,
                _ => return None,
            };
            Some((a, ctl.asset_overview(a), source))
        });
        if let Some((_, None, _)) = &ready {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let title = format!("Cart Setup — {}", display_label(&cart, &snap.project));
        let out_labels = self.output_labels(&cart.output);
        let meter = self.meters.get(&id).copied().unwrap_or_default();
        let rt = snap.runtime.get(&id).copied().unwrap_or_default();
        let playing = matches!(rt.state, CartState::Playing | CartState::Stopping);
        let mut ok = false;
        let mut cancel = false;
        let mut prompt = self.setup_close_prompt;
        let (open, f) = native_window(
            ctx,
            WindowSpec {
                key: "audio-setup",
                title: &title,
                size: [980.0, 780.0],
                min_size: [820.0, 600.0],
                on_top: false,
                fit: false,
            },
            |ui| {
                // Space plays/stops the preview; I and O set In and Out (at the playhead,
                // or under the mouse when stopped). Not while typing in a field.
                // P plays the last seconds before Out, to hear the loop point.
                let (space, key_i, key_o, key_p) = if ui.ctx().egui_wants_keyboard_input() {
                    (false, false, false, false)
                } else {
                    ui.ctx().input_mut(|i| {
                        (
                            i.consume_key(egui::Modifiers::NONE, Key::Space),
                            i.consume_key(egui::Modifiers::NONE, Key::I),
                            i.consume_key(egui::Modifiers::NONE, Key::O),
                            i.consume_key(egui::Modifiers::NONE, Key::P),
                        )
                    })
                };
                self.keyboard(ui.ctx(), snap, intents);
                let mut play_pause = space;
                let before = (c.gain_db, c.muted, c.speed, c.looping);
                let changed = c != cart;

                // OK / Cancel, always at the bottom.
                egui::Panel::bottom("setup-buttons")
                    .exact_size(52.0)
                    .frame(
                        egui::Frame::new()
                            .fill(if prompt {
                                Color32::from_rgb(0xF0, 0x9A, 0x2A).gamma_multiply(0.2)
                            } else {
                                ui.visuals().panel_fill
                            })
                            .inner_margin(egui::Margin::symmetric(12, 10)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            if prompt {
                                ui.label(
                                    egui::RichText::new("Keep your changes before closing?")
                                        .strong(),
                                );
                            } else if changed {
                                ui.label(
                                    egui::RichText::new("Changes are kept when you press OK.")
                                        .weak(),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let okb = egui::Button::new(
                                        egui::RichText::new(if prompt { "Keep" } else { "OK" })
                                            .strong()
                                            .color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(0x2F, 0x6B, 0xFF))
                                    .min_size(Vec2::new(96.0, 30.0));
                                    if ui.add(okb).clicked() {
                                        ok = true;
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(if prompt {
                                                "Discard"
                                            } else {
                                                "Cancel"
                                            })
                                            .min_size(Vec2::new(90.0, 30.0)),
                                        )
                                        .clicked()
                                    {
                                        cancel = true;
                                    }
                                },
                            );
                        });
                    });

                egui::CentralPanel::default().show(ui, |ui| {
                    let full_w = ui.available_width();
                    // ---- header: name and file details; waveform tools on the right ----
                    let mut tools = (false, false); // (top and tail, zoom to fit)
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(display_label(&c, &snap.project))
                                .strong()
                                .size(15.0),
                        );
                        if let Some((_, _, source)) = &ready {
                            let full = source.frames as f64 / source.sample_rate.max(1) as f64;
                            let file = cart
                                .asset
                                .and_then(|a| snap.project.asset(a))
                                .map(|a| {
                                    a.path.rsplit(['/', '\\']).next().unwrap_or("").to_string()
                                })
                                .unwrap_or_default();
                            ui.label(
                                egui::RichText::new(format!(
                                    "· {file} · {} kHz · {} ch · {}",
                                    source.sample_rate as f32 / 1000.0,
                                    source.channels,
                                    fmt_precise(full as f32)
                                ))
                                .weak(),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            tools.1 = ui.button("Zoom to fit").clicked();
                            tools.0 = ui
                                .add_enabled(
                                    matches!(ready, Some((_, Some(_), _))),
                                    egui::Button::new("Top and tail"),
                                )
                                .on_hover_text("Trim silence below −40 dBFS")
                                .clicked();
                        });
                    });
                    ui.add_space(6.0);
                    if tools.1 {
                        self.wave_view = WaveView::default();
                    }
                    match &ready {
                        None => {
                            let info = cart
                                .asset
                                .and_then(|a| snap.asset_info.get(&a).cloned())
                                .unwrap_or_else(|| "No audio file assigned.".into());
                            ui.label(info);
                        }
                        Some((_, overview, source)) => {
                            let src_rate = source.sample_rate.max(1) as f64;
                            let full = source.frames as f64 / src_rate;
                            let peaks = overview
                                .clone()
                                .unwrap_or_else(|| Arc::new(Overview::placeholder(full)));
                            let (mut in_s, mut out_s) = match c.trim {
                                Some(t) => (
                                    t.in_frames as f64 / src_rate,
                                    t.out_frames as f64 / src_rate,
                                ),
                                None => (0.0, full),
                            };
                            let fade_in_s = c.fade_in.map_or(0.0, |f| f.ms as f64 / 1000.0);
                            let fade_out_s = c
                                .fade_out
                                .map_or(snap.project.settings.default_fade_out_ms, |f| f.ms)
                                as f64
                                / 1000.0;
                            let playhead =
                                playing.then_some(in_s + rt.progress as f64 * (out_s - in_s));
                            let height = (ui.available_height() - 440.0).clamp(110.0, 320.0);
                            let mut changed_w = false;
                            // ---- waveform with the meter beside it, the same height ----
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let w = ui.available_width() - 64.0 - 8.0;
                                ui.allocate_ui(Vec2::new(w, height), |ui| {
                                    changed_w = waveform_ui(
                                        ui,
                                        &peaks,
                                        &mut self.wave_view,
                                        TrimEdit {
                                            in_s: &mut in_s,
                                            out_s: &mut out_s,
                                            fade_in_s,
                                            fade_out_s: if c.fade_out_at_end {
                                                fade_out_s
                                            } else {
                                                0.0
                                            },
                                            playhead_s: playhead,
                                        },
                                        height,
                                    );
                                });
                                ppm_ui(ui, &meter, height);
                            });
                            let mut num_changed = changed_w;
                            if tools.0
                                && let Some((a, b)) = peaks.audio_bounds()
                            {
                                in_s = a;
                                out_s = b;
                                num_changed = true;
                            }
                            let at = playhead.or(self.wave_view.hover_s);
                            if key_i && let Some(t) = at {
                                in_s = t;
                                num_changed = true;
                            }
                            if key_o && let Some(t) = at {
                                out_s = t;
                                num_changed = true;
                            }
                            ui.add_space(10.0);
                            // ---- trim in · transport · trim out: three boxes, same height ----
                            let nudge = if ui.input(|i| i.modifiers.shift) {
                                0.1
                            } else {
                                0.01
                            };
                            let gap = 10.0;
                            let mid = 260.0;
                            let side = ((full_w - mid - 2.0 * gap) / 2.0).max(180.0);
                            let box_h = 92.0;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                trim_box_sized(
                                    ui,
                                    "Trim in",
                                    &mut in_s,
                                    0.0,
                                    full,
                                    nudge,
                                    &mut num_changed,
                                    0.0,
                                    side,
                                    box_h,
                                );
                                egui::Frame::group(ui.style())
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        let inner = mid - 16.0;
                                        ui.set_width(inner);
                                        ui.set_height(box_h - 16.0);
                                        let pos = if playing { rt.elapsed_s } else { 0.0 };
                                        let rem = if playing {
                                            rt.remaining_s
                                        } else {
                                            ((out_s - in_s).max(0.0) / c.speed.max(0.01) as f64)
                                                as f32
                                        };
                                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                let rw = (inner - 6.0) / 2.0;
                                                setup_ui::readout(
                                                    ui,
                                                    &fmt_precise(pos),
                                                    Color32::from_rgb(0xF5, 0xD0, 0x20),
                                                    rw,
                                                );
                                                setup_ui::readout(
                                                    ui,
                                                    &format!("-{}", fmt_precise(rem)),
                                                    Color32::from_rgb(0x4F, 0xD1, 0xFF),
                                                    rw,
                                                );
                                            });
                                            ui.horizontal(|ui| {
                                                let bw = Vec2::new((inner - 18.0) / 4.0, 32.0);
                                                if icon_button_sized(ui, Icon::ToIn, bw)
                                                    .on_hover_text("Back to In")
                                                    .clicked()
                                                {
                                                    intents.push(Intent::KillCart(id));
                                                    self.setup_paused = false;
                                                }
                                                if ui
                                                    .add_sized(
                                                        bw,
                                                        egui::Button::selectable(c.looping, "Loop"),
                                                    )
                                                    .on_hover_text(
                                                        "Loop. P plays the last 3 s before Out, so \
                                                         you hear the loop point; ⌥-click (Alt-click) \
                                                         or middle-click the waveform to play from there.",
                                                    )
                                                    .clicked()
                                                {
                                                    c.looping = !c.looping;
                                                }
                                                let running = playing && !self.setup_paused;
                                                let (icon, tip) = if running {
                                                    (Icon::Pause, "Pause (Space)")
                                                } else {
                                                    (Icon::Play, "Play (Space)")
                                                };
                                                if icon_button_sized(ui, icon, bw)
                                                    .on_hover_text(tip)
                                                    .clicked()
                                                {
                                                    play_pause = true;
                                                }
                                                if icon_button_sized(ui, Icon::Stop, bw)
                                                    .on_hover_text("Stop")
                                                    .clicked()
                                                {
                                                    intents.push(Intent::KillCart(id));
                                                    self.setup_paused = false;
                                                }
                                            });
                                        });
                                    });
                                trim_box_sized(
                                    ui,
                                    "Trim out",
                                    &mut out_s,
                                    0.0,
                                    full,
                                    nudge,
                                    &mut num_changed,
                                    full,
                                    side,
                                    box_h,
                                );
                            });
                            if num_changed {
                                in_s = in_s.clamp(0.0, full);
                                out_s = out_s.clamp(in_s + 0.001, full);
                                let in_f = (in_s * src_rate).round() as u64;
                                let out_f = ((out_s * src_rate).round() as u64).min(source.frames);
                                c.trim = if in_f == 0 && out_f >= source.frames {
                                    None
                                } else {
                                    Some(bubble_project::Trim {
                                        in_frames: in_f,
                                        out_frames: out_f.max(in_f + 1),
                                    })
                                };
                                // Heard straight away while auditioning.
                                if playing {
                                    intents.push(Intent::LiveTrim(id, in_s, out_s));
                                }
                            }
                            // ⌥/Alt- or middle-click in the waveform plays from there; P
                            // plays the last 3 s before Out to hear the loop point.
                            let mut from = self.wave_view.seek_to.take().map(|t| (t, false));
                            if key_p {
                                from = Some(((out_s - 3.0).max(in_s), true));
                            }
                            if let Some((t, resume)) = from {
                                let t = t.clamp(in_s, (out_s - 0.001).max(in_s));
                                if playing {
                                    intents.push(Intent::Seek(id, t));
                                    if resume && self.setup_paused {
                                        intents.push(Intent::LiveSpeedRamp(id, c.speed, 5));
                                        self.setup_paused = false;
                                    }
                                } else {
                                    intents.push(Intent::PreviewAt(c.clone(), t));
                                    self.setup_paused = false;
                                }
                            }
                        }
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(4.0);
                    // ---- settings: two columns, one label column, one right edge ----
                    let default_out = snap.project.settings.default_fade_out_ms;
                    ui.columns(2, |cols| {
                        let ui = &mut cols[0];
                        ui.label(egui::RichText::new("Sound").strong().size(15.0));
                        ui.add_space(4.0);
                        form_row(ui, "Output", |ui| {
                            ui.spacing_mut().combo_width = ui.available_width();
                            output_combo(ui, "setup-output", &out_labels, &mut c.output)
                        });
                        form_row(ui, "Gain", |ui| {
                            setup_ui::slider_row(
                                ui,
                                &mut c.gain_db,
                                (-12.0, 12.0),
                                0.0,
                                false,
                                |v| format!("{v:.1} dB").replace('-', "−"),
                                setup_ui::parse_db,
                                0.1,
                            );
                        });
                        form_row(ui, "Pan", |ui| {
                            setup_ui::slider_row(
                                ui,
                                &mut c.pan,
                                (-1.0, 1.0),
                                0.0,
                                false,
                                setup_ui::pan_text,
                                setup_ui::parse_pan,
                                0.01,
                            );
                        });
                        form_row(ui, "Speed", |ui| {
                            setup_ui::slider_row(
                                ui,
                                &mut c.speed,
                                (0.25, 4.0),
                                1.0,
                                true,
                                |v| format!("{:.0}%", v * 100.0),
                                parse_percent,
                                0.005,
                            );
                        });
                        form_row(ui, "Speed bar", |ui| {
                            ui.label(egui::RichText::new("x0").weak());
                            seconds_combo(ui, "setup-x0", &mut c.slowdown_ms, false);
                            ui.label(egui::RichText::new("x1").weak());
                            seconds_combo(ui, "setup-x1", &mut c.speedup_ms, true);
                            if ui.button("Open…").clicked() {
                                self.open_speed_bar(id);
                            }
                        });
                        form_row(ui, "Fade in", |ui| {
                            fade_picker_ex(ui, "setup-fade-in", &mut c.fade_in, None, false)
                        });
                        form_row(ui, "Fade out", |ui| {
                            fade_picker_ex(
                                ui,
                                "setup-fade-out",
                                &mut c.fade_out,
                                Some(default_out),
                                false,
                            )
                        });
                        form_row(ui, "Playback", |ui| {
                            ui.checkbox(&mut c.looping, "Loop");
                            ui.checkbox(&mut c.muted, "Mute");
                            ui.checkbox(&mut c.hold_to_play, "Hold");
                        });
                        form_row(ui, "On retrigger", |ui| {
                            let mut rtg = c
                                .retrigger
                                .unwrap_or(snap.project.settings.default_retrigger);
                            egui::ComboBox::from_id_salt("setup-retrigger")
                                .selected_text(match rtg {
                                    Retrigger::Restart => "Restart",
                                    Retrigger::Overlap => "Overlap",
                                    Retrigger::ToggleStop => "Toggle stop",
                                    Retrigger::Ignore => "Ignore",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut rtg, Retrigger::Restart, "Restart");
                                    ui.selectable_value(&mut rtg, Retrigger::Overlap, "Overlap");
                                    ui.selectable_value(
                                        &mut rtg,
                                        Retrigger::ToggleStop,
                                        "Toggle stop",
                                    );
                                    ui.selectable_value(&mut rtg, Retrigger::Ignore, "Ignore");
                                });
                            c.retrigger =
                                (rtg != snap.project.settings.default_retrigger).then_some(rtg);
                        });

                        let ui = &mut cols[1];
                        ui.label(egui::RichText::new("Cart").strong().size(15.0));
                        ui.add_space(4.0);
                        form_row(ui, "Label", |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut c.label)
                                    .hint_text(display_label(&cart, &snap.project))
                                    .desired_width(f32::INFINITY),
                            );
                        });
                        form_row(ui, "Colour", |ui| {
                            colour_swatches(ui, &mut c.color, &mut self.setup_picker)
                        });
                        form_row(ui, "When it starts", |ui| {
                            egui::ComboBox::from_id_salt("setup-stop-others")
                                .selected_text(match c.on_play.stop {
                                    StopScope::None => "Stop nothing",
                                    StopScope::All => "Stop all other carts",
                                    StopScope::Page => "Stop others on this page",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut c.on_play.stop,
                                        StopScope::None,
                                        "Stop nothing",
                                    );
                                    ui.selectable_value(
                                        &mut c.on_play.stop,
                                        StopScope::All,
                                        "Stop all other carts",
                                    );
                                    ui.selectable_value(
                                        &mut c.on_play.stop,
                                        StopScope::Page,
                                        "Stop others on this page",
                                    );
                                });
                        });
                        form_row(ui, "When it ends", |ui| {
                            let mut next = c.play_next.is_some();
                            if ui.checkbox(&mut next, "Play the next cart").changed() {
                                c.play_next = next.then(|| "next_cell".to_string());
                            }
                        });
                        form_row(ui, "Group", |ui| {
                            group_combo(ui, "setup-group", &snap.project, &mut c.group)
                        });
                        form_row(ui, "Trigger", |ui| {
                            ui.vertical(|ui| self.triggers_ui(ui, snap, id, intents));
                        });
                        form_row(ui, "Audio file", |ui| {
                            if let Some((page, row, col)) = snap.project.cart_position(id)
                                && ui.button("Replace…").clicked()
                            {
                                self.files.request(Purpose::Assign {
                                    page,
                                    row,
                                    col,
                                    many: false,
                                });
                            }
                            if let Some(a) = c.asset
                                && snap.missing.contains(&a)
                                && ui.button("Locate…").clicked()
                            {
                                self.files.request(Purpose::Relink(a));
                            }
                        });
                        form_row(ui, "Notes", |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut c.notes)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });
                });
                // Play / pause (button or Space). Pause holds the playhead (speed 0).
                if play_pause {
                    if playing && !self.setup_paused {
                        intents.push(Intent::LiveSpeedRamp(id, 0.0, 5));
                        self.setup_paused = true;
                    } else if playing {
                        intents.push(Intent::LiveSpeedRamp(id, c.speed, 5));
                        self.setup_paused = false;
                    } else {
                        intents.push(Intent::Preview(c.clone()));
                        self.setup_paused = false;
                    }
                }
                // Heard while auditioning: gain, mute and speed act on the playing cart.
                if playing && !self.setup_paused {
                    if (c.gain_db, c.muted) != (before.0, before.1) {
                        intents.push(Intent::LiveGain(id, c.gain_db, c.muted));
                    }
                    if c.speed != before.2 {
                        intents.push(Intent::LiveSpeedRamp(id, c.speed, 40));
                    }
                }
                // Loop on/off applies to the preview straight away, paused or not.
                if playing && c.looping != before.3 {
                    intents.push(Intent::LiveLoop(id, c.looping));
                }
            },
        );
        if !playing {
            self.setup_paused = false;
        }
        if std::mem::take(&mut self.focus_setup) {
            ctx.send_viewport_cmd_to(
                egui::ViewportId::from_hash_of("audio-setup"),
                egui::ViewportCommand::Focus,
            );
        }
        self.child_focus |= f;
        let changed = c != cart;
        if ok {
            if changed {
                intents.push(Intent::ReplaceCart(c));
            }
            self.close_setup();
        } else if cancel {
            if playing {
                intents.push(Intent::KillCart(id));
            }
            self.close_setup();
        } else if !open && changed {
            // Closing with unapplied edits asks first (the bar turns orange).
            prompt = true;
            self.setup_close_prompt = prompt;
            self.setup_draft = Some(c);
        } else if !open {
            self.close_setup();
        } else {
            self.setup_close_prompt = prompt;
            self.setup_draft = Some(c);
        }
    }

    fn close_setup(&mut self) {
        self.dialogs.audio_setup = None;
        self.setup_draft = None;
        self.setup_close_prompt = false;
        self.setup_picker = false;
    }

    fn close_guard(&mut self, ctx: &egui::Context, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if ctx.input(|i| i.viewport().close_requested()) && snap.dirty && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            // The prompt is in the main window: bring it in front of setup windows.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.dialogs.confirm_close = true;
        }
        if self.dialogs.confirm_close {
            let resp = egui::Modal::new(egui::Id::unique("confirm-close")).show(ctx, |ui| {
                ui.heading("Quit Bubble?");
                ui.label("The project has unsaved changes.");
                crate::native::dialog_bar(ui, |ui| {
                    if ui.add(crate::native::primary("Save and quit")).clicked() {
                        self.save_or_save_as(snap, intents);
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add(crate::native::secondary("Cancel")).clicked() {
                        self.dialogs.confirm_close = false;
                    }
                    if ui
                        .add(crate::native::secondary("Quit without saving"))
                        .clicked()
                    {
                        self.discard_on_exit = true;
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            if resp.should_close() {
                self.dialogs.confirm_close = false;
            }
        }
    }
}
