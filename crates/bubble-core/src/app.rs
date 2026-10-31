//! Bubble's application state and logic: the open session, audio outputs, MIDI,
//! file dialogs, background jobs, autosave and recovery, and the [`Intent`]s an
//! interface sends. An interface owns a [`Core`], draws from [`Core::snapshot`], and
//! answers the questions in [`Core::prompts`]; nothing here draws.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bubble_control::{
    AssetState, AttrSet, CartClip, CartRuntime, Controller, InputEvent, MouseButton, TriggerSource,
};
use bubble_engine::device::{AudioOutput, OutputDeviceInfo, list_output_devices};
use bubble_project::{Action, Cart, ExclusiveGroup, Project, ProjectSettings};
use uuid::Uuid;

use crate::autosave::{autosave_path, recovery_dir};
use crate::dialogs::{FileDialogs, Purpose};
use crate::midi::{MidiInputs, available_ports};
use crate::prefs::Prefs;
use crate::writer::{WriteDone, WriteJob, Writer};
use crate::{cloud, crash};

pub mod audio_out;
pub mod convert;
pub mod session;

pub use session::{AfterCreate, Job, JobControl, JobResult, NewSession, saved_text};

/// A project being read in the background: (path, opened at start-up?, result).
pub type PendingOpen = (
    PathBuf,
    bool,
    std::sync::mpsc::Receiver<Result<(Project, bubble_control::assets::Resolved), String>>,
);

pub const AUTOSAVE_EVERY: Duration = Duration::from_secs(30);
/// How often a version is kept for Restore from Backup while there are new edits.
const BACKUP_EVERY: Duration = Duration::from_secs(5 * 60);
/// An edit is autosaved once this long has passed without another.
pub const AUTOSAVE_QUIET: Duration = Duration::from_secs(2);

/// Changes requested by the UI during a frame, applied after rendering.
#[allow(clippy::large_enum_variant)] // short-lived, a few per frame
pub enum Intent {
    RestoreBackup(PathBuf),
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
    /// Start a new, empty session saved at this file (its folder already exists), with
    /// its name and whether audio added to it is converted to WAV.
    CreateSession(PathBuf, String, bool),
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
    /// Close the session (its unsaved changes already answered for): an empty,
    /// untitled one takes its place, and nothing reopens at the next launch.
    CloseSession,
    Paste(usize, u16, u16, CartClip),
    PasteAttrs(Cart, Vec<Uuid>, AttrSet),
}

pub struct Snapshot {
    pub project: Project,
    pub runtime: HashMap<Uuid, CartRuntime>,
    pub page: usize,
    pub dirty: bool,
    pub path: Option<PathBuf>,
    pub learning: Option<bubble_control::Learn>,
    pub pending: usize,
    pub missing: Vec<Uuid>,
    pub asset_info: HashMap<Uuid, String>,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Clone)]
pub enum PendingAction {
    New,
    Open(PathBuf),
    /// Import a SpotOn session or package into a new session.
    Import(PathBuf),
    /// File ▸ Close Session: an empty cart wall, as at a first launch.
    Close,
    /// Session ▸ Restore from Backup: that version, into the session.
    Restore(PathBuf),
}

/// Questions and messages for the person using Bubble. The interface shows whichever
/// are set and answers through the [`Core`] methods named on each field.
#[derive(Default)]
pub struct Prompts {
    /// A problem to report (dismiss by setting it to `None`).
    pub error: Option<String>,
    /// Autosaves found at start-up: (autosave file, session it belongs to).
    pub recover: Vec<(PathBuf, Option<PathBuf>)>,
    /// A SpotOn file to import: add to the open session ([`Core::import_into_current`])
    /// or start a new one ([`Core::perform`] with [`PendingAction::Import`]).
    pub import_choice: Option<PathBuf>,
    /// Files to offer converting to WAV ([`Core::start_conversion`]).
    pub convert_offer: Option<Vec<(Uuid, PathBuf)>>,
    /// Unsaved changes would be lost: confirm with [`Core::perform`].
    pub confirm_discard: Option<PendingAction>,
    /// The New Session form ([`Core::create_session`] when confirmed).
    pub new_session: Option<NewSession>,
}

/// How often the engines' reports are handled, and the wait for the session's lock
/// worth a line in the log.
const EVENT_PUMP_EVERY: Duration = Duration::from_millis(10);
const LOCK_WAIT_LOGGED: Duration = Duration::from_millis(20);

/// When edits were last seen, for the autosave.
struct AutosaveState {
    seen: u64,
    changed: Instant,
    /// Since when there have been edits not yet autosaved.
    waiting: Option<Instant>,
}

/// How the session was before one being loaded, to go back to on Cancel.
pub(crate) struct Revert {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    saved_at: Option<std::time::SystemTime>,
    last_project: Option<PathBuf>,
}

/// What follows a save, once it has succeeded.
pub enum AfterSave {
    Perform(PendingAction),
    Quit,
}

/// Everything Bubble does that is not drawing. Fields are public so interfaces can show
/// them; change them through the methods.
pub struct Core {
    pub ctrl: Arc<Mutex<Controller>>,
    /// Output peaks per playing cart (linear), gathered by every snapshot and kept
    /// until a meter takes them: a snapshot taken for another reason must not lose
    /// them.
    peaks: Mutex<HashMap<Uuid, (f32, f32)>>,
    /// To go back to if the session being loaded is cancelled.
    pub(crate) revert: Option<Revert>,
    /// Cancelled: on the way back to the session before. The dialog stays until there.
    pub reverting: bool,
    /// When a version was last kept for Restore from Backup, and at which edit.
    backup_at: Instant,
    backup_edits: u64,
    /// Times audio was late or missing this run, and when it last happened.
    pub dropouts: u64,
    pub dropout_at: Option<Instant>,
    /// A long file that could not be read while playing, and when.
    pub read_failure: Option<(String, Instant)>,
    /// When an output that stopped was last reopened by itself.
    pub audio_recovered: Option<Instant>,
    /// Writes sessions, autosaves and preferences on its own thread.
    pub writer: Writer,
    /// Windows wants to end the session and has been held back.
    session_end_asked: Arc<std::sync::atomic::AtomicBool>,
    /// An autosave is on its way to the disk: the next waits for it.
    autosave_writing: bool,
    /// What to do once the save being written has succeeded (a failed one does neither,
    /// so unsaved work is not thrown away).
    pub after_save: Option<AfterSave>,
    /// The session was saved as part of quitting: the interface should close now.
    pub quit_after_save: bool,
    /// One running stream per output device (output A's device first).
    pub audio: Vec<AudioOutput>,
    /// The device each stream was asked for ("" = system default), parallel to `audio`.
    pub audio_keys: Vec<String>,
    /// Devices that are missing, and the check for their return.
    pub watch: audio_out::DeviceWatch,
    pub audio_error: Option<String>,
    /// Outputs that could not be opened ("Output B: …").
    pub output_errors: Vec<String>,
    pub midi: MidiInputs,
    pub midi_ports: Vec<String>,
    /// MIDI being listed and opened on a helper thread: Windows can take seconds to
    /// list its MIDI devices (10 s seen on a Surface), and nothing should wait for it.
    pub midi_opening: Option<std::sync::mpsc::Receiver<(Vec<String>, MidiInputs, Instant)>>,
    pub prefs: Prefs,
    pub prompts: Prompts,
    /// A cart the interface should select (`Some(None)`: clear the selection).
    pub select: Option<Option<Uuid>>,
    autosave: AutosaveState,
    /// Wakes the interface (e.g. when a background job finishes).
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub files: FileDialogs,
    pub clipboard: Option<CartClip>,
    /// Copied page (Copy Page / Paste Page).
    pub page_clip: Option<bubble_control::PageClip>,
    /// Session being read in the background.
    pub opening: Option<PendingOpen>,
    /// Identifies this run's untitled recovery file.
    pub session_id: u64,
    pub started: Instant,
    /// A session or SpotOn file named on the command line, opened on the first frame.
    pub open_at_start: Option<PathBuf>,
    /// Requests from later launches (see [`crate::instance`]).
    pub requests: std::sync::mpsc::Receiver<crate::instance::Request>,
    /// When the open session started loading its audio (logged once it is ready).
    pub loading_since: Option<Instant>,
    /// Most audio files seen waiting at once, for the "n of total" progress.
    pub loading_total: usize,
    /// The last autosave or preferences save failed (shown until one succeeds).
    pub autosave_problem: Option<String>,
    pub prefs_problem: Option<String>,
    /// The open session is in a cloud-synced folder (logged), when that was last
    /// checked, and whether the fuller alert was shown for this session.
    pub cloud_status: Option<String>,
    pub cloud_checked: Option<Instant>,
    cloud_job: Option<std::sync::mpsc::Receiver<Option<cloud::CloudCheck>>>,
    pub cloud_alerted: bool,
    /// Set when the user chose to quit without saving.
    pub discard_on_exit: bool,
    pub devices: Vec<OutputDeviceInfo>,
    /// A device scan running on a helper thread (enumeration can take seconds on
    /// Windows with Bluetooth or virtual devices).
    pub devices_scan: Option<std::sync::mpsc::Receiver<Vec<OutputDeviceInfo>>>,
    /// Background copy/import in progress.
    pub job: Option<Job>,
    /// Collect outside audio into the session once nothing else is running.
    pub want_collect: bool,
    /// A WAV conversion check is due (`true`: offer it first).
    pub wav_check: Option<bool>,
    /// When the session file was last written (for the title).
    pub saved_at: Option<std::time::SystemTime>,
}

impl Core {
    /// A core with an untitled session and nothing opened yet; call [`Core::start`]
    /// once the interface exists. `repaint` wakes the interface from other threads.
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let ctrl = Arc::new(Mutex::new(Controller::new(
            Controller::new_project("Untitled"),
            None,
        )));
        let core = Core {
            ctrl,
            peaks: Mutex::new(HashMap::new()),
            audio: Vec::new(),
            audio_keys: Vec::new(),
            watch: Default::default(),
            audio_error: None,
            output_errors: Vec::new(),
            midi: MidiInputs::empty(),
            // Listed in `start`, after the window is up (it can take a while).
            midi_ports: Vec::new(),
            midi_opening: None,
            revert: None,
            reverting: false,
            backup_at: Instant::now(),
            backup_edits: 0,
            dropouts: 0,
            dropout_at: None,
            read_failure: None,
            audio_recovered: None,
            writer: Writer::new(repaint.clone()),
            session_end_asked: Arc::default(),
            autosave_writing: false,
            after_save: None,
            quit_after_save: false,
            prefs: Prefs::load(),
            prompts: Prompts::default(),
            select: None,
            autosave: AutosaveState {
                seen: 0,
                changed: Instant::now(),
                waiting: None,
            },
            files: FileDialogs::new(repaint.clone()),
            clipboard: None,
            page_clip: None,
            opening: None,
            session_id: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            started: Instant::now(),
            open_at_start: None,
            requests: {
                let r = repaint.clone();
                crate::instance::watch(move || r())
            },
            loading_since: None,
            loading_total: 0,
            autosave_problem: None,
            prefs_problem: None,
            cloud_status: None,
            cloud_checked: None,
            cloud_job: None,
            cloud_alerted: false,
            discard_on_exit: false,
            devices: Vec::new(),
            devices_scan: None,
            job: None,
            want_collect: false,
            wav_check: None,
            saved_at: None,
            repaint,
        };
        core.apply_machine();
        core
    }

    /// Gives the controller this machine's output level, limiter and mouse mode.
    pub fn apply_machine(&self) {
        let m = self.prefs.machine();
        self.lock().set_machine(m);
    }

    /// Changes this machine's output level, limiter or mouse mode, and keeps it.
    pub fn edit_machine(&mut self, f: impl FnOnce(&mut Prefs)) {
        f(&mut self.prefs);
        self.apply_machine();
        self.save_prefs();
    }

    /// Start-up: crash reporting, audio, MIDI, recovery of unsaved work, and the
    /// session to open (a file given at launch, or the last one).
    pub fn start(&mut self) {
        let t = Instant::now();
        // Each step is timed in the log: the window waits for none of them, but a slow
        // device shows here (Windows took 11 s before the window existed).
        let step = |what: &str, since: Instant| {
            crate::log::info(&format!(
                "start-up: {what} in {:.2} s",
                since.elapsed().as_secs_f32()
            ));
        };
        crash::install(self.ctrl.clone(), self.session_id);
        #[cfg(windows)]
        crash::install_native(self.ctrl.clone(), self.session_id);
        self.scan_devices();
        // Opened in the background; `watch_audio` takes them when ready.
        self.open_audio();
        self.start_event_pump();
        // Listed and opened in the background; `poll_midi` takes them when ready.
        self.open_midi();
        let s = Instant::now();
        self.find_recoverable();
        step("recovery files checked", s);
        if let Some(report) = crash::take_unreported() {
            self.prompts.error = Some(format!(
                "Bubble quit unexpectedly last time.\n\nA crash report was saved to:\n{}\n\n\
                 Any unsaved work it could keep is offered for recovery.",
                report.display()
            ));
        }
        // A file given at launch (dropped on Bubble.exe, Open With → Bubble) opens
        // instead of the last session.
        self.open_at_start = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .find(|p| p.is_file());
        if self.open_at_start.is_none()
            && let Some(p) = self.prefs.last_project.clone()
        {
            self.start_open(p, true);
        }
        step("all of start-up", t);
    }

    /// Takes a finished device scan, if any.
    pub fn poll_devices(&mut self) {
        if let Some(rx) = &self.devices_scan
            && let Ok(list) = rx.try_recv()
        {
            self.devices = list;
            self.devices_scan = None;
        }
    }

    /// Lists the output devices on a helper thread; the result arrives on a later frame.
    pub fn scan_devices(&mut self) {
        if self.devices_scan.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::Builder::new()
            .name("bubble-devices".into())
            .spawn(move || {
                let _ = tx.send(list_output_devices().unwrap_or_default());
                repaint();
            })
            .expect("spawn device scan");
        self.devices_scan = Some(rx);
    }

    /// Warns when the open session lives in a cloud-synced folder, and clearly when
    /// any of its audio is online-only (checked on open and every minute).
    pub fn check_cloud(&mut self) {
        if let Some(rx) = &self.cloud_job {
            match rx.try_recv() {
                Ok(found) => {
                    self.cloud_job = None;
                    self.apply_cloud_check(found);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.cloud_job = None,
            }
            return;
        }
        if self.opening.is_some()
            || self
                .cloud_checked
                .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        self.cloud_checked = Some(Instant::now());
        let (project, dir, path) = {
            let c = self.lock();
            (
                c.project().clone(),
                c.project_dir(),
                c.path().map(Path::to_path_buf),
            )
        };
        let Some(path) = path else {
            self.cloud_status = None;
            return;
        };
        // Every audio file is looked at on disk, perhaps a slow or missing one.
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("bubble-cloud-check".into())
            .spawn(move || {
                let audio: Vec<PathBuf> = bubble_control::asset_files_of(&project, dir.as_deref())
                    .into_iter()
                    .map(|(_, p)| p)
                    .collect();
                let _ = tx.send(cloud::check(&path, &audio));
                repaint();
            });
        if spawned.is_ok() {
            self.cloud_job = Some(rx);
        }
    }

    fn apply_cloud_check(&mut self, found: Option<cloud::CloudCheck>) {
        let status = found.as_ref().map(|c| c.status.clone());
        if status != self.cloud_status
            && let Some(s) = &status
        {
            crate::log::info(s);
        }
        self.cloud_status = status;
        if let Some(alert) = found.and_then(|c| c.alert)
            && !self.cloud_alerted
        {
            self.cloud_alerted = true;
            self.prompts.error = Some(alert);
        }
    }

    /// Saves preferences; a failure is logged and shown in the status bar until a
    /// later save succeeds.
    pub fn save_prefs(&mut self) {
        if let Some((path, bytes)) = self.prefs.encoded() {
            self.writer.submit(WriteJob::Prefs { path, bytes });
        }
    }

    /// Handles the engines' reports on a thread of its own: Play Next must not wait for
    /// the interface. Also logs when the session's lock made it wait, the same wait a
    /// MIDI trigger would have had.
    fn start_event_pump(&self) {
        let ctrl = Arc::downgrade(&self.ctrl);
        let repaint = self.repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("bubble-events".into())
            .spawn(move || {
                let mut logged = None::<Instant>;
                while let Some(ctrl) = ctrl.upgrade() {
                    let t = Instant::now();
                    let mut c = ctrl.lock().unwrap_or_else(|p| p.into_inner());
                    let waited = t.elapsed();
                    let any = c.pump_events();
                    drop(c);
                    drop(ctrl);
                    if any {
                        repaint();
                    }
                    if waited > LOCK_WAIT_LOGGED
                        && logged.is_none_or(|l: Instant| l.elapsed() > Duration::from_secs(1))
                    {
                        logged = Some(Instant::now());
                        crate::log::info(&format!("session lock waited {} ms", waited.as_millis()));
                    }
                    std::thread::sleep(EVENT_PUMP_EVERY);
                }
            });
        if let Err(e) = spawned {
            crate::log::info(&format!(
                "no event thread ({e}); events follow the interface"
            ));
        }
    }

    /// Takes the results of finished file writes. Call once per frame.
    pub fn poll_writes(&mut self) {
        for done in self.writer.finished() {
            match done {
                WriteDone::Session { path, result } => {
                    let ok = result.is_ok();
                    let same = {
                        let mut c = self.lock();
                        c.save_finished(&path, ok);
                        c.path() == Some(path.as_path())
                    };
                    match result {
                        Ok(()) => {
                            if same {
                                self.saved_at = Some(std::time::SystemTime::now());
                            }
                            match self.after_save.take() {
                                Some(AfterSave::Perform(action)) => {
                                    let mut intents = Vec::new();
                                    self.perform(action, &mut intents);
                                    self.apply(intents);
                                }
                                Some(AfterSave::Quit) => self.quit_after_save = true,
                                None => {}
                            }
                        }
                        Err(e) => {
                            crate::log::info(&format!("save failed: {e}"));
                            self.after_save = None;
                            self.prompts.error = Some(e);
                        }
                    }
                }
                WriteDone::Autosave(result) => {
                    self.autosave_writing = false;
                    match result {
                        Ok(()) => self.autosave_problem = None,
                        Err(e) => {
                            let msg = format!("Autosave failed: {e}");
                            crate::log::info(&msg);
                            self.autosave_problem = Some(msg);
                        }
                    }
                }
                WriteDone::Prefs(result) => match result {
                    Ok(()) => self.prefs_problem = None,
                    Err(e) => {
                        let msg = format!("Preferences could not be saved: {e}");
                        crate::log::info(&msg);
                        self.prefs_problem = Some(msg);
                    }
                },
            }
        }
    }

    /// Keeps unsaved work when Windows ends the session (restart, log off): it does not
    /// wait for Bubble, so this writes on the spot.
    pub fn saver_for_session_end(&self) -> impl Fn() + Send + Sync + 'static {
        let (ctrl, id) = (self.ctrl.clone(), self.session_id);
        move || {
            crate::log::info("Windows is ending the session");
            for _ in 0..20 {
                match crash::autosave_now(&ctrl, id) {
                    busy if busy.starts_with(crash::BUSY) => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    done => {
                        crate::log::info(&format!("unsaved work: {done}"));
                        return;
                    }
                }
            }
        }
    }

    /// Asked when Windows wants to end the session: whether to hold it back. With unsaved
    /// work it does (the interface then asks), having autosaved first in case the user
    /// makes Windows go on anyway. A shutdown Windows will not wait for is not held.
    pub fn holder_for_session_end(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        let (ctrl, id) = (self.ctrl.clone(), self.session_id);
        let (held, repaint) = (self.session_end_asked.clone(), self.repaint.clone());
        move || {
            let dirty = match ctrl.try_lock() {
                Ok(c) => c.is_dirty(),
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().is_dirty(),
                // Busy for a moment: better to ask than to lose work.
                Err(std::sync::TryLockError::WouldBlock) => true,
            };
            if !dirty {
                return false;
            }
            let saved = crash::autosave_now(&ctrl, id);
            crate::log::info(&format!(
                "holding back Windows' shutdown; autosave: {saved}"
            ));
            held.store(true, std::sync::atomic::Ordering::SeqCst);
            repaint();
            true
        }
    }

    /// Whether a shutdown has just been held back, for the interface to ask.
    pub fn session_end_held(&self) -> bool {
        self.session_end_asked
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    /// Waits for files still being written (Bubble is quitting).
    pub fn finish_writes(&self) {
        self.writer.finish();
    }

    /// The file of each asset on disk, without holding the session's lock meanwhile.
    pub fn asset_files(&self) -> Vec<(Uuid, PathBuf)> {
        let (project, dir) = {
            let c = self.lock();
            (c.project().clone(), c.project_dir())
        };
        bubble_control::asset_files_of(&project, dir.as_deref())
    }

    /// The autosaves of the session in the controller.
    fn autosave_files(&self, c: &Controller) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Some(p) = c.path() {
            files.push(autosave_path(p));
        }
        if let Some(d) = recovery_dir() {
            files.push(d.join(format!("Untitled-{}.bubble", self.session_id)));
        }
        files
    }

    pub fn lock(&self) -> MutexGuard<'_, Controller> {
        self.ctrl.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// "Output A", "Output A - PA", or with `detail` the device and channels.
    /// Labels for every output a cart can pick (plus `extra`'s, if not set up here).
    pub fn output_labels(&self, extra: &str) -> Vec<String> {
        let n = self
            .prefs
            .effective_outputs()
            .len()
            .max(bubble_project::output_index(extra).map_or(0, |i| i + 1));
        (0..n).map(|i| self.output_label(i, false)).collect()
    }

    pub fn output_label(&self, index: usize, detail: bool) -> String {
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

    /// Lists the MIDI ports and opens the chosen ones on a helper thread (the current
    /// inputs close now); `poll_midi` takes the result. The newest request wins.
    pub fn open_midi(&mut self) {
        self.midi = MidiInputs::empty();
        let (tx, rx) = std::sync::mpsc::channel();
        let (ctrl, repaint) = (self.ctrl.clone(), self.repaint.clone());
        let (disabled, wanted) = (self.prefs.midi_disabled, self.prefs.midi_inputs.clone());
        std::thread::Builder::new()
            .name("bubble-midi-open".into())
            .spawn(move || {
                let t = Instant::now();
                let ports = available_ports();
                let midi = if disabled {
                    MidiInputs::empty()
                } else {
                    let r = repaint.clone();
                    MidiInputs::open(&ctrl, &wanted, move || r())
                };
                let _ = tx.send((ports, midi, t));
                repaint();
            })
            .expect("spawn MIDI thread");
        self.midi_opening = Some(rx);
    }

    /// Takes MIDI once the helper thread has opened it (call every refresh).
    pub fn poll_midi(&mut self) {
        let Some(rx) = &self.midi_opening else { return };
        match rx.try_recv() {
            Ok((ports, midi, t)) => {
                crate::log::info(&format!(
                    "MIDI ready in {:.2} s: {} port(s), {} open",
                    t.elapsed().as_secs_f32(),
                    ports.len(),
                    midi.open_ports().len()
                ));
                for e in &midi.errors {
                    crate::log::info(&format!("MIDI problem: {e}"));
                }
                self.midi_ports = ports;
                self.midi = midi;
                self.midi_opening = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(_) => self.midi_opening = None,
        }
    }

    /// The peaks seen since the last call (linear, per playing cart), for a meter.
    pub fn take_peaks(&self) -> HashMap<Uuid, (f32, f32)> {
        let fresh = self.lock().take_cart_peaks();
        let mut acc = self.peaks.lock().unwrap_or_else(|e| e.into_inner());
        merge_peaks(&mut acc, fresh);
        std::mem::take(&mut *acc)
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut c = self.lock();
        merge_peaks(
            &mut self.peaks.lock().unwrap_or_else(|e| e.into_inner()),
            c.take_cart_peaks(),
        );
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

    pub fn apply(&mut self, intents: Vec<Intent>) {
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
                        self.select = Some(Some(first));
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
                        self.select = Some(Some(id));
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
                Intent::CreateSession(file, name, wav) => {
                    c.replace_project(Controller::new_project(&name), None);
                    if wav {
                        c.edit_settings(|s| s.convert_to_wav = true);
                    }
                    self.select = Some(None);
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
                Intent::SaveAsWith(p, moves) => match c.begin_save_as_with(&p, &moves) {
                    Ok(bytes) => {
                        let then_remove = recovery_dir()
                            .map(|d| d.join(format!("Untitled-{}.bubble", self.session_id)))
                            .into_iter()
                            .collect();
                        drop(c);
                        self.writer.submit(WriteJob::Session {
                            path: p.clone(),
                            bytes,
                            then_remove,
                        });
                        self.prefs.add_recent(p);
                        reopen_prefs = true;
                        continue;
                    }
                    Err(e) => Err(e.to_string()),
                },
                Intent::RestoreBackup(file) => {
                    drop(c);
                    match bubble_project::io::load(&file) {
                        Ok(project) => {
                            self.remember_for_revert();
                            self.loading_since = Some(Instant::now());
                            let mut c = self.lock();
                            let path = c.path().map(Path::to_path_buf);
                            c.replace_project(project, path);
                            c.set_dirty();
                            drop(c);
                            self.select = Some(None);
                            crate::log::info(&format!("restored {}", file.display()));
                        }
                        Err(e) => {
                            self.prompts.error =
                                Some(format!("Could not restore that backup:\n{e}"));
                        }
                    }
                    continue;
                }
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
                Intent::CloseSession => {
                    // Changes were saved or given up: their autosaves go too.
                    let files = self.autosave_files(&c);
                    self.writer.submit(WriteJob::Remove(files));
                    c.replace_project(Controller::new_project("Untitled"), None);
                    self.select = Some(None);
                    self.saved_at = None;
                    self.prefs.last_project = None;
                    reopen_prefs = true;
                    crate::log::info("session closed");
                    Ok(())
                }
                Intent::Redo => {
                    c.redo();
                    Ok(())
                }
                Intent::Paste(p, r, col, clip) => {
                    if let Some(id) = c.paste_cart(p, r, col, &clip) {
                        self.select = Some(Some(id));
                    }
                    Ok(())
                }
                Intent::PasteAttrs(from, targets, what) => {
                    c.paste_attributes(&from, &targets, what);
                    Ok(())
                }
                Intent::Save => match c.begin_save() {
                    Ok((path, bytes)) => {
                        let then_remove = self.autosave_files(&c);
                        let backups = crate::backups::folder(c.project().id);
                        drop(c);
                        if let Some(dir) = backups {
                            self.writer.submit(WriteJob::Backup {
                                dir,
                                bytes: bytes.clone(),
                            });
                        }
                        self.backup_at = Instant::now();
                        self.writer.submit(WriteJob::Session {
                            path,
                            bytes,
                            then_remove,
                        });
                        continue;
                    }
                    Err(e) => {
                        self.after_save = None;
                        Err(e.to_string())
                    }
                },
            };
            drop(c);
            if let Err(e) = result {
                self.prompts.error = Some(e);
            }
        }
        if reopen_prefs {
            self.save_prefs();
        }
    }

    /// Keeps the session's current state as a version for Restore from Backup.
    pub fn keep_backup(&mut self) {
        self.backup_at = Instant::now();
        let (id, bytes) = {
            let c = self.lock();
            (
                c.project().id,
                bubble_project::io::to_json_string(c.project()).into_bytes(),
            )
        };
        if let Some(dir) = crate::backups::folder(id) {
            self.writer.submit(WriteJob::Backup { dir, bytes });
        }
    }

    /// Keeps the session as it is, for Cancel to go back to.
    fn remember_for_revert(&mut self) {
        let (project, path, dirty) = {
            let c = self.lock();
            (
                c.project().clone(),
                c.path().map(Path::to_path_buf),
                c.is_dirty(),
            )
        };
        self.revert = Some(Revert {
            project,
            path,
            dirty,
            saved_at: self.saved_at,
            last_project: self.prefs.last_project.clone(),
        });
    }

    /// Whether a session is on its way in: the loading dialog is up, and input waits.
    pub fn session_loading(&self) -> bool {
        self.opening.is_some()
            || self.loading_since.is_some()
            || self.reverting
            || self.job.as_ref().is_some_and(|j| j.control.in_bytes)
    }

    /// Cancel in the loading dialog: back to how things were before it began.
    pub fn cancel_load(&mut self) {
        if self.reverting {
            return;
        }
        if self.job.as_ref().is_some_and(|j| j.control.in_bytes) {
            self.cancel_job();
        } else if self.opening.take().is_some() {
            crate::log::info("cancelled opening a session");
        } else if let Some(r) = self.revert.take() {
            crate::log::info("cancelled loading a session; going back");
            self.loading_since = Some(Instant::now());
            self.reverting = true;
            let mut c = self.lock();
            c.replace_project(r.project, r.path);
            if r.dirty {
                c.set_dirty();
            }
            drop(c);
            self.saved_at = r.saved_at;
            self.select = Some(None);
            self.prefs.last_project = r.last_project;
            self.save_prefs();
        }
    }

    /// Reads a project on a helper thread; `poll_open` applies it.
    pub fn start_open(&mut self, path: PathBuf, startup: bool) {
        crate::log::info(&format!("opening {}", path.display()));
        let (tx, rx) = std::sync::mpsc::channel();
        let repaint = self.repaint.clone();
        let p = path.clone();
        std::thread::Builder::new()
            .name("bubble-open".into())
            .spawn(move || {
                let r = bubble_project::io::load(&p).map_err(|e| e.to_string());
                // Its files are looked up here, off the session's lock.
                let r = r.map(|project| {
                    let found = bubble_control::assets::resolve_all(&project, p.parent());
                    (project, found)
                });
                let _ = tx.send(r);
                repaint();
            })
            .expect("spawn open thread");
        self.opening = Some((path, startup, rx));
    }

    pub fn poll_open(&mut self) {
        let Some((path, startup, rx)) = &self.opening else {
            return;
        };
        let Ok(result) = rx.try_recv() else { return };
        let (path, startup) = (path.clone(), *startup);
        self.opening = None;
        match result {
            Ok((project, found)) => {
                crate::log::info(&format!(
                    "opened: {} page(s), {} audio file(s)",
                    project.pages.len(),
                    project.assets.len()
                ));
                if self.prefs.adopt_from_session(&project.settings) {
                    crate::log::info("output level, limiter or mouse mode taken from the session");
                    self.apply_machine();
                }
                self.loading_since = Some(Instant::now());
                self.cloud_checked = None;
                self.cloud_alerted = false;
                self.remember_for_revert();
                {
                    let mut c = self.lock();
                    c.use_resolved(found);
                    c.replace_project(project, Some(path.clone()));
                }
                self.select = Some(None);
                self.saved_at = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                self.want_collect = true;
                self.want_wav_check(true);
                self.prefs.add_recent(path);
                self.save_prefs();
                self.find_recoverable();
            }
            // A last project that no longer exists is not worth an error at start-up.
            Err(_) if startup && !path.exists() => {}
            Err(e) => {
                crate::log::info(&format!("could not open {}: {e}", path.display()));
                self.prompts.error = Some(format!("Could not open {}:\n{e}", path.display()));
            }
        }
    }

    pub fn autosave(&mut self) {
        self.poll_writes();
        let (edits, dirty) = {
            let c = self.lock();
            (c.edit_count(), c.is_dirty())
        };
        // A version for Restore from Backup, every so often while there are new edits.
        if edits != self.backup_edits && self.backup_at.elapsed() >= BACKUP_EVERY {
            self.backup_edits = edits;
            self.keep_backup();
        }
        if edits != self.autosave.seen {
            self.autosave.seen = edits;
            self.autosave.changed = Instant::now();
            self.autosave.waiting.get_or_insert_with(Instant::now);
        }
        if !dirty {
            self.autosave.waiting = None;
        }
        let Some(waiting) = self.autosave.waiting else {
            return;
        };
        // Soon after an edit settles; in a long run of edits, still every so often.
        if !self.autosave_writing
            && (self.autosave.changed.elapsed() >= AUTOSAVE_QUIET
                || waiting.elapsed() >= AUTOSAVE_EVERY)
        {
            self.autosave.waiting = None;
            self.write_autosave();
        }
    }

    /// Writes unsaved changes next to the project (`<file>.autosave`) or, for an
    /// untitled project, into the recovery folder. Never overwrites the project file.
    pub fn write_autosave(&mut self) {
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
        if let Some(path) = target {
            self.autosave_writing = true;
            self.writer.submit(WriteJob::Autosave {
                path,
                bytes: json.into_bytes(),
            });
        }
    }

    pub fn remove_autosave(&self) {
        let files = self.autosave_files(&self.lock());
        self.writer.submit(WriteJob::Remove(files));
    }

    /// Looks for unsaved work left by a previous session.
    pub fn find_recoverable(&mut self) {
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
        self.prompts.recover = found;
    }

    pub fn save_or_save_as(&mut self, snap: &Snapshot, intents: &mut Vec<Intent>) {
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
    pub fn file_results(&mut self, snap: &Snapshot, intents: &mut Vec<Intent>) {
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

    pub fn request(&mut self, a: PendingAction, snap: &Snapshot, intents: &mut Vec<Intent>) {
        if snap.dirty {
            self.prompts.confirm_discard = Some(a);
        } else {
            self.perform(a, intents);
        }
    }

    pub fn perform(&mut self, a: PendingAction, intents: &mut Vec<Intent>) {
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
            PendingAction::Close => intents.push(Intent::CloseSession),
            PendingAction::Restore(p) => intents.push(Intent::RestoreBackup(p)),
        }
    }

    pub fn reopen_midi(&mut self) {
        self.save_prefs();
        self.open_midi();
    }
}

/// Keeps the larger peak of each channel per cart.
fn merge_peaks(acc: &mut HashMap<Uuid, (f32, f32)>, fresh: HashMap<Uuid, (f32, f32)>) {
    for (id, (l, r)) in fresh {
        let e = acc.entry(id).or_default();
        e.0 = e.0.max(l);
        e.1 = e.1.max(r);
    }
}
