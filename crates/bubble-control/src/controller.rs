//! The control layer.
//!
//! All inputs (pointer, keyboard, MIDI) end up in [`Controller::trigger`] /
//! [`Controller::stop_cart`] and friends, which translate project settings into
//! engine commands. The controller never touches audio-thread state directly: it
//! sends [`EngineCommand`]s and reads [`bubble_engine::EngineStatus`] atomics.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bubble_engine::loader::SourceInfo;
use bubble_engine::{
    CartKey, EndReason, EngineCommand, EngineEvent, EngineHandle, Fade, FadeCurve, PlayParams,
    StopOthers, StopTarget, VoicePhase, VoiceView, db_to_gain,
};
use bubble_project::{
    Action, AssetMeta, Binding, BindingSource, Cart, Control, Curve, DuplicatePolicy,
    ExclusiveGroup, Extra, FadeSpec, MouseMode, Page, Project, Retrigger, Scope, StopScope,
    io as pio, make_asset_ref, output_index, relink,
};
use uuid::Uuid;

use crate::assets::{AssetCache, AssetState};
use crate::input::{Hit, InputEvent, TriggerSource, matches, source_from_event};

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("{0}")]
    Load(#[from] pio::LoadError),
    #[error("{0}")]
    Save(#[from] pio::SaveError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// What the UI should show for a cart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CartState {
    /// No audio assigned.
    #[default]
    Empty,
    Loading,
    Missing,
    Error,
    Ready,
    Playing,
    Stopping,
}

/// An asset converted to WAV (see [`Controller::apply_conversions`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversion {
    pub asset: Uuid,
    /// The converted file.
    pub file: PathBuf,
    /// Sample rate of the file it replaces.
    pub from_rate: u32,
    pub to_rate: u32,
}

/// Where an output (A, B, …) plays: an engine (device) and a channel range on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    /// Index into the engines passed to [`Controller::attach_outputs`].
    pub engine: usize,
    /// First device channel (0-based).
    pub offset: u16,
    /// 1 (mono), 2 (stereo) or 6 (5.1).
    pub channels: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CartRuntime {
    pub state: CartState,
    pub voices: u32,
    pub looping: bool,
    /// 0…1 through the In–Out range of the most recent voice.
    pub progress: f32,
    pub elapsed_s: f32,
    pub remaining_s: f32,
    pub duration_s: f32,
    /// The file on disk differs from the stored fingerprint.
    pub changed: bool,
    /// Current playback speed of the most recent voice (1 = normal).
    pub speed: f32,
    /// The speed bar's `x0` is slowing or has halted this cart.
    pub slowed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Primary,
    Secondary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogKind {
    Started(TriggerSource),
    Ended(EndReason),
    Ignored,
    Rejected(String),
    Info(String),
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub time: SystemTime,
    pub cart: Option<Uuid>,
    pub label: String,
    pub kind: LogKind,
}

const LOG_CAPACITY: usize = 2000;

/// Pending "learn" request: the next input becomes a binding for this cart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Learn {
    pub cart: Uuid,
    pub action: Action,
}

pub struct Controller {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    /// One engine per output device; empty without audio.
    engines: Vec<EngineHandle>,
    /// Output letter (index) → where it plays.
    routes: Vec<Route>,
    assets: AssetCache,
    keys: HashMap<Uuid, CartKey>,
    by_key: HashMap<u32, Uuid>,
    next_key: u32,
    current_page: usize,
    log: VecDeque<LogEntry>,
    learn: Option<Learn>,
    /// Carts slowed (or halted) by the speed bar's `x0`.
    slowed: std::collections::HashSet<Uuid>,
    /// Round-robin cursor for overlapped bindings.
    overlap_cursor: HashMap<String, usize>,
    undo: Vec<Project>,
    redo: Vec<Project>,
    /// Key and time of the last checkpoint, for coalescing rapid edits.
    last_checkpoint: Option<(String, std::time::Instant)>,
    batch: bool,
}

const UNDO_DEPTH: usize = 100;
const COALESCE: std::time::Duration = std::time::Duration::from_millis(1000);

/// A copied cart: its settings plus the asset it plays.
#[derive(Clone, Debug)]
pub struct CartClip {
    pub cart: Cart,
    pub asset: Option<bubble_project::AssetRef>,
}

/// Which carts "Reassign Outputs" changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReassignScope {
    All,
    Page(usize),
    Group(Uuid),
    /// Buttons `first..=last` (1-based, reading order) on a page.
    Buttons {
        page: usize,
        first: u32,
        last: u32,
    },
}

/// A copied page: its layout and carts (with their audio references).
#[derive(Clone, Debug)]
pub struct PageClip {
    pub name: String,
    pub rows: u16,
    pub cols: u16,
    pub carts: Vec<(u16, u16, CartClip)>,
}

/// Which attributes "Paste Attributes" copies (REF "Paste Special").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttrSet {
    pub gain: bool,
    pub pan: bool,
    pub fades: bool,
    pub colour: bool,
    pub looping: bool,
    pub behaviour: bool,
}

impl AttrSet {
    pub const ALL: AttrSet = AttrSet {
        gain: true,
        pan: true,
        fades: true,
        colour: true,
        looping: true,
        behaviour: true,
    };
}

impl Controller {
    pub fn new(project: Project, path: Option<PathBuf>) -> Self {
        let mut c = Controller {
            project,
            path,
            dirty: false,
            engines: Vec::new(),
            routes: Vec::new(),
            assets: AssetCache::new(),
            keys: HashMap::new(),
            by_key: HashMap::new(),
            next_key: 1,
            current_page: 0,
            log: VecDeque::new(),
            learn: None,
            slowed: Default::default(),
            overlap_cursor: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_checkpoint: None,
            batch: false,
        };
        c.rebuild_keys();
        c
    }

    /// A new project with sensible default bindings (Esc = stop all).
    pub fn new_project(name: &str) -> Project {
        let mut p = Project::new(name);
        p.bindings.push(Binding {
            id: Uuid::new_v4(),
            source: BindingSource::Key {
                key: "Escape".into(),
                modifiers: vec![],
            },
            action: Action::StopAll,
            targets: vec![],
            policy: None,
            scope: None,
            velocity_to_gain: false,
            extra: Extra::new(),
        });
        p
    }

    // ----- project / file -------------------------------------------------

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn project_dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Marks the project as having unsaved changes (e.g. after restoring an autosave).
    pub fn set_dirty(&mut self) {
        self.dirty = true;
    }

    /// Replaces the project (New/Open): stops audio and re-arms all assets.
    pub fn replace_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.stop_all(false);
        self.project = project;
        self.path = path;
        self.dirty = false;
        self.current_page = 0;
        self.learn = None;
        self.rebuild_keys();
        self.rearm();
        self.info(format!("Opened project \"{}\"", self.project.name));
    }

    pub fn open(&mut self, path: &Path) -> Result<(), ControlError> {
        let p = pio::load(path)?;
        self.replace_project(p, Some(path.to_path_buf()));
        Ok(())
    }

    /// Saves to the current path. Fails if the project has never been saved.
    pub fn save(&mut self) -> Result<(), ControlError> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| ControlError::Invalid("project has no file yet".into()))?;
        pio::save(&mut self.project, &path)?;
        self.dirty = false;
        self.info(format!("Saved {}", path.display()));
        Ok(())
    }

    /// Saves to a new location, rewriting asset paths relative to it.
    pub fn save_as(&mut self, path: &Path) -> Result<(), ControlError> {
        let new_dir = std::path::absolute(path.parent().unwrap_or(Path::new(".")))?;
        let old_dir = self.project_dir();
        let mut copy = self.project.clone();
        bubble_project::resolve::rebase_paths(&mut copy, old_dir.as_deref(), &new_dir);
        pio::save(&mut copy, path)?;
        self.project = copy;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.info(format!("Saved {}", path.display()));
        Ok(())
    }

    // ----- engine ---------------------------------------------------------

    /// Connects one engine as output A (stereo, channels 1–2).
    pub fn attach_engine(&mut self, handle: EngineHandle) {
        self.attach_outputs(
            vec![handle],
            vec![Route {
                engine: 0,
                offset: 0,
                channels: 2,
            }],
        );
    }

    /// Connects one engine per device and the routes for outputs A, B, … All engines
    /// must run at the same sample rate (assets are prepared once, at that rate).
    pub fn attach_outputs(&mut self, engines: Vec<EngineHandle>, routes: Vec<Route>) {
        debug_assert!(
            engines
                .windows(2)
                .all(|w| w[0].status().sample_rate() == w[1].status().sample_rate()),
            "all output devices must share one sample rate"
        );
        self.engines = engines;
        self.routes = routes;
        let rate = self.rate();
        self.apply_master();
        if rate > 0 && self.assets.rate() != rate {
            self.rearm();
        }
    }

    /// Disconnects every engine (the caller drops the streams).
    pub fn detach_engines(&mut self) -> Vec<EngineHandle> {
        self.routes.clear();
        std::mem::take(&mut self.engines)
    }

    /// The first engine (output A's device), for diagnostics.
    pub fn engine(&self) -> Option<&EngineHandle> {
        self.engines.first()
    }

    pub fn engines(&self) -> &[EngineHandle] {
        &self.engines
    }

    /// Number of outputs (A, B, …) currently available.
    pub fn output_count(&self) -> usize {
        self.routes.len()
    }

    fn rate(&self) -> u32 {
        self.engines.first().map_or(0, |e| e.status().sample_rate())
    }

    /// Where a cart plays: its output letter's route, or output A when that letter
    /// is not set up on this machine.
    fn route_for(&self, cart: &Cart) -> Route {
        output_index(&cart.output)
            .and_then(|i| self.routes.get(i))
            .or(self.routes.first())
            .copied()
            .unwrap_or(Route {
                engine: 0,
                offset: 0,
                channels: 2,
            })
    }

    /// Whether any voice is sounding on any output.
    pub fn any_voice(&self) -> bool {
        let mut any = false;
        self.for_each_voice(|_| any = true);
        any
    }

    fn for_each_voice(&self, mut f: impl FnMut(VoiceView)) {
        for e in &self.engines {
            e.status().for_each_voice(&mut f);
        }
    }

    fn rearm(&mut self) {
        let rate = self.rate();
        if rate > 0 {
            let dir = self.project_dir();
            self.assets.rearm_all(&self.project, dir.as_deref(), rate);
        }
    }

    fn apply_master(&mut self) {
        let gain = db_to_gain(self.project.settings.master_gain_db);
        let limiter = self.project.settings.limiter;
        self.send(EngineCommand::SetMaster { gain });
        self.send(EngineCommand::SetLimiter { enabled: limiter });
    }

    /// Sends to every engine (stops, gain, speed… reach a cart wherever it plays).
    fn send(&mut self, cmd: EngineCommand) -> bool {
        if self.engines.is_empty() {
            return false;
        }
        let n = self.engines.len();
        let mut ok = true;
        for i in 0..n {
            ok &= self.send_to(i, cmd.clone());
        }
        ok
    }

    fn send_to(&mut self, engine: usize, cmd: EngineCommand) -> bool {
        let Some(e) = self.engines.get_mut(engine) else {
            return false;
        };
        match e.send(cmd) {
            Ok(()) => true,
            Err(_) => {
                self.log_push(None, String::new(), LogKind::Rejected("engine busy".into()));
                false
            }
        }
    }

    /// Housekeeping, call regularly (e.g. every UI frame): finishes asset loads,
    /// processes engine events (logging, play-next), releases retired buffers.
    pub fn tick(&mut self) {
        for (asset, info) in self.assets.poll() {
            if let (Some(info), Some(a)) = (info, self.project.asset_mut(asset)) {
                let meta = a.meta.get_or_insert_with(AssetMeta::default);
                meta.sample_rate = info.sample_rate;
                meta.channels = info.channels;
                meta.frames = info.frames;
            }
        }
        self.end_halted();
        let mut events = Vec::new();
        for e in &mut self.engines {
            e.poll_events(|ev| events.push(ev));
        }
        for ev in events {
            match ev {
                EngineEvent::VoiceStarted { .. } => {}
                EngineEvent::VoiceEnded { cart, reason, .. } => {
                    let Some(id) = self.by_key.get(&cart.0).copied() else {
                        continue;
                    };
                    let label = self.label(id);
                    self.log_push(Some(id), label, LogKind::Ended(reason));
                    if reason == EndReason::Completed {
                        self.play_next(id);
                    }
                }
                EngineEvent::PlayIgnored { cart } => {
                    if let Some(id) = self.by_key.get(&cart.0).copied() {
                        let label = self.label(id);
                        self.log_push(Some(id), label, LogKind::Ignored);
                    }
                }
            }
        }
    }

    /// (bytes held by prepared audio, bytes allowed).
    pub fn audio_memory(&self) -> (u64, u64) {
        self.assets.memory()
    }

    /// Number of assets still being prepared.
    pub fn assets_pending(&self) -> usize {
        self.assets.pending()
    }

    pub fn asset_state(&self, asset: Uuid) -> Option<&AssetState> {
        self.assets.state(asset)
    }

    /// Asset ids that could not be found on disk.
    pub fn missing_assets(&self) -> Vec<Uuid> {
        let mut v = self.assets.missing();
        v.sort_by_key(|id| self.project.assets.iter().position(|a| a.id == *id));
        v
    }

    // ----- runtime view ---------------------------------------------------

    /// Per-cart display state for every cart. Cheap enough to call once per frame.
    pub fn runtime(&self) -> HashMap<Uuid, CartRuntime> {
        let rate = self.rate().max(1) as f32;
        let mut out: HashMap<Uuid, CartRuntime> = HashMap::new();
        for (_, cell) in self.project.carts() {
            let c = &cell.cart;
            let mut rt = CartRuntime::default();
            if c.control.is_some() {
                rt.state = CartState::Ready;
            }
            if let Some(a) = c.asset {
                match self.assets.state(a) {
                    Some(AssetState::Ready {
                        buffer,
                        changed,
                        source,
                        ..
                    }) => {
                        rt.state = CartState::Ready;
                        rt.changed = *changed;
                        // Playable length: trimmed range at the preset speed.
                        let full = buffer.duration_secs() as f32;
                        let src = source.sample_rate.max(1) as f32;
                        let len = match c.trim {
                            Some(t) => (t.out_frames.saturating_sub(t.in_frames)) as f32 / src,
                            None => full,
                        };
                        rt.duration_s = len / c.speed.max(0.01);
                        rt.speed = c.speed;
                    }
                    Some(AssetState::Loading { .. }) => rt.state = CartState::Loading,
                    Some(AssetState::Missing { .. }) => rt.state = CartState::Missing,
                    Some(AssetState::Error { .. }) => rt.state = CartState::Error,
                    None => {
                        rt.state = if !self.engines.is_empty() {
                            CartState::Loading
                        } else {
                            CartState::Ready
                        }
                    }
                }
            }
            out.insert(c.id, rt);
        }
        {
            self.for_each_voice(|v| {
                let Some(id) = self.by_key.get(&v.cart.0) else {
                    return;
                };
                let Some(rt) = out.get_mut(id) else { return };
                rt.voices += 1;
                let stopping = v.phase == VoicePhase::Stopping;
                if !stopping {
                    rt.state = CartState::Playing;
                    rt.slowed = self.slowed.contains(id);
                } else if rt.state != CartState::Playing {
                    rt.state = CartState::Stopping;
                }
                rt.looping |= v.looping;
                let len = v.length_frames.max(1) as f32;
                let pos = v.position_frames as f32;
                // Show the most recent (least advanced) voice.
                let progress = pos / len;
                if rt.voices == 1 || progress < rt.progress {
                    // Wall-clock times at the current speed (source time when stopped).
                    let sp = if v.speed > 0.01 { v.speed } else { 1.0 };
                    rt.progress = progress;
                    rt.elapsed_s = pos / rate / sp;
                    rt.remaining_s = (len - pos).max(0.0) / rate / sp;
                    rt.duration_s = len / rate / sp;
                    rt.speed = v.speed;
                }
            });
        }
        out
    }

    fn is_playing(&self, cart: Uuid) -> Option<f32> {
        let key = self.keys.get(&cart)?;
        let rate = self.rate().max(1) as f32;
        let mut remaining: Option<f32> = None;
        self.for_each_voice(|v| {
            if v.cart == *key && v.phase != VoicePhase::Stopping {
                let r = (v.length_frames.saturating_sub(v.position_frames)) as f32 / rate;
                let r = if v.looping { f32::INFINITY } else { r };
                remaining = Some(remaining.map_or(r, |x: f32| x.min(r)));
            }
        });
        remaining
    }

    pub fn current_page(&self) -> usize {
        self.current_page
    }

    pub fn set_current_page(&mut self, page: usize) {
        if page < self.project.pages.len() {
            self.current_page = page;
        }
    }

    pub fn log(&self) -> &VecDeque<LogEntry> {
        &self.log
    }

    // ----- triggering -----------------------------------------------------

    /// Mouse/touch press on a cart.
    pub fn pointer_down(&mut self, cart: Uuid, button: MouseButton) {
        match (self.project.settings.mouse_mode, button) {
            (MouseMode::Toggle, MouseButton::Primary) => {
                if self.resume_if_slowed(cart) {
                } else if self.is_playing(cart).is_some() {
                    self.stop_cart(cart, true);
                } else {
                    self.trigger(cart, TriggerSource::Mouse, None);
                }
            }
            (MouseMode::LeftPlayRightStop, MouseButton::Primary) => {
                self.trigger(cart, TriggerSource::Mouse, None);
            }
            (_, MouseButton::Secondary) => self.stop_cart(cart, true),
        }
    }

    /// Mouse/touch release: stops hold-to-play carts.
    pub fn pointer_up(&mut self, cart: Uuid) {
        if self.project.cart(cart).is_some_and(|c| c.hold_to_play) {
            self.stop_cart(cart, true);
        }
    }

    /// Plays a cart using its settings. `velocity` (MIDI) scales gain when enabled.
    pub fn trigger(&mut self, cart: Uuid, source: TriggerSource, velocity: Option<u8>) -> bool {
        if self.resume_if_slowed(cart) {
            return true;
        }
        let Some((page_idx, row, col)) = self.project.cart_position(cart) else {
            return false;
        };
        let c = self.project.pages[page_idx]
            .cell(row, col)
            .expect("position from project")
            .cart
            .clone();
        self.trigger_cart(c, page_idx, source, velocity)
    }

    /// Plays a cart with settings that are not saved yet (Cart Setup's preview), on
    /// the cart's own voice so its normal stop and meters apply.
    pub fn preview(&mut self, draft: &Cart) -> bool {
        let Some((page_idx, _, _)) = self.project.cart_position(draft.id) else {
            return false;
        };
        self.trigger_cart(draft.clone(), page_idx, TriggerSource::Ui, None)
    }

    fn trigger_cart(
        &mut self,
        c: Cart,
        page_idx: usize,
        source: TriggerSource,
        velocity: Option<u8>,
    ) -> bool {
        let cart = c.id;
        let label = display_label(&c, &self.project);
        if let Some(control) = c.control {
            return self.run_control(&c, control, page_idx, label, source);
        }
        let Some(asset) = c.asset else {
            return false;
        };
        let reject = match self.assets.state(asset) {
            Some(AssetState::Ready { .. }) => None,
            Some(AssetState::Missing { .. }) => Some("file missing"),
            Some(AssetState::Error { .. }) => Some("file cannot be decoded"),
            Some(AssetState::Loading { .. }) | None => Some("still loading"),
        };
        if self.engines.is_empty() {
            self.log_push(
                Some(cart),
                label,
                LogKind::Rejected("no audio output".into()),
            );
            return false;
        }
        if let Some(r) = reject {
            self.log_push(Some(cart), label, LogKind::Rejected(r.into()));
            return false;
        }
        let Some(AssetState::Ready {
            buffer,
            source: info,
            ..
        }) = self.assets.state(asset)
        else {
            return false;
        };
        let buffer = buffer.clone();
        let route = self.route_for(&c);
        let mut params = self.play_params(&c, page_idx, *info, velocity);
        params.output_offset = route.offset;
        params.output_channels = route.channels;
        let key = self.key(cart);
        // Exclusive group and stop-others also act on the other devices' voices.
        for i in (0..self.engines.len()).filter(|i| *i != route.engine) {
            if params.group != 0 {
                self.send_to(
                    i,
                    EngineCommand::Stop {
                        target: StopTarget::Group(params.group),
                        fade: None,
                    },
                );
            }
            let target = match params.stop_others {
                StopOthers::None => None,
                StopOthers::All => Some(StopTarget::All),
                StopOthers::Page => Some(StopTarget::Page(params.page)),
            };
            if let Some(target) = target {
                self.send_to(
                    i,
                    if params.stop_others_fade {
                        EngineCommand::Stop { target, fade: None }
                    } else {
                        EngineCommand::Kill { target }
                    },
                );
            }
        }
        if self.send_to(
            route.engine,
            EngineCommand::Play {
                cart: key,
                buffer,
                params,
            },
        ) {
            self.log_push(Some(cart), label, LogKind::Started(source));
            true
        } else {
            false
        }
    }

    /// A control button: stops its group, its page, or everything (each cart with its
    /// own fade-out).
    fn run_control(
        &mut self,
        c: &Cart,
        control: Control,
        page_idx: usize,
        label: String,
        source: TriggerSource,
    ) -> bool {
        match control {
            Control::StopGroup => {
                let Some(g) = c
                    .group
                    .and_then(|g| self.project.groups.iter().position(|x| x.id == g))
                else {
                    self.log_push(Some(c.id), label, LogKind::Rejected("no group set".into()));
                    return false;
                };
                self.send(EngineCommand::Stop {
                    target: StopTarget::Group(g as u32 + 1),
                    fade: None,
                });
            }
            Control::StopPage => self.stop_page(page_idx),
            Control::StopAll => self.stop_all(true),
        }
        self.log_push(Some(c.id), label, LogKind::Started(source));
        true
    }

    /// Makes a cell a control button (replacing what is there): red, labelled after
    /// what it does, and in `group` for Stop Group.
    pub fn create_control(
        &mut self,
        page: usize,
        row: u16,
        col: u16,
        control: Control,
        group: Option<Uuid>,
    ) -> Option<Uuid> {
        let (rows, cols) = self.project.pages.get(page).map(|p| (p.rows, p.cols))?;
        if row >= rows || col >= cols {
            return None;
        }
        self.checkpoint("control");
        let p = &mut self.project.pages[page];
        p.cells.retain(|c| !(c.row == row && c.col == col));
        let mut cart = Cart::new();
        cart.control = Some(control);
        cart.label = match control {
            Control::StopGroup => "Stop".into(),
            Control::StopPage => "Stop Page".into(),
            Control::StopAll => "Stop All".into(),
        };
        cart.color = Some("#C62828".into());
        cart.group = group;
        let id = cart.id;
        p.cells.push(bubble_project::Cell { row, col, cart });
        self.key(id);
        self.project.prune_unused_assets();
        self.mark_dirty();
        Some(id)
    }

    fn play_params(
        &self,
        c: &Cart,
        page_idx: usize,
        src: SourceInfo,
        velocity: Option<u8>,
    ) -> PlayParams {
        let rate = self.rate();
        let s = &self.project.settings;
        let mut gain = if c.muted { 0.0 } else { db_to_gain(c.gain_db) };
        if let Some(v) = velocity {
            gain *= v as f32 / 127.0;
        }
        let ratio = rate as f64 / src.sample_rate.max(1) as f64;
        let (start_frame, end_frame) = match c.trim {
            Some(t) => (
                (t.in_frames as f64 * ratio).round() as usize,
                Some((t.out_frames as f64 * ratio).round() as usize),
            ),
            None => (0, None),
        };
        let fade = |f: FadeSpec| Fade {
            frames: (f.ms as u64 * rate as u64 / 1000) as u32,
            curve: match f.curve {
                Curve::Linear => FadeCurve::Linear,
                Curve::EqualPower => FadeCurve::EqualPower,
                Curve::Exponential => FadeCurve::Exponential,
            },
        };
        let retrigger = match c.retrigger.unwrap_or(s.default_retrigger) {
            Retrigger::Restart => bubble_engine::Retrigger::Restart,
            Retrigger::Overlap => bubble_engine::Retrigger::Overlap,
            Retrigger::ToggleStop => bubble_engine::Retrigger::ToggleStop,
            Retrigger::Ignore => bubble_engine::Retrigger::Ignore,
        };
        let group = c
            .group
            .and_then(|g| self.project.groups.iter().position(|x| x.id == g))
            .map_or(0, |i| i as u32 + 1);
        PlayParams {
            gain,
            pan: c.pan,
            start_frame,
            end_frame,
            looping: c.looping,
            fade_in: c.fade_in.map(fade).unwrap_or(Fade::NONE),
            fade_out: fade(c.fade_out.unwrap_or(FadeSpec::ms(s.default_fade_out_ms))),
            retrigger,
            group,
            page: page_idx as u32 + 1,
            stop_others: match c.on_play.stop {
                StopScope::None => StopOthers::None,
                StopScope::All => StopOthers::All,
                StopScope::Page => StopOthers::Page,
            },
            stop_others_fade: c.on_play.stop_fade,
            speed: c.speed,
            // Set from the cart's output route by `trigger`.
            ..PlayParams::default()
        }
    }

    /// Stops a cart (with its fade-out, or a 2 ms ramp when `fade` is false).
    pub fn stop_cart(&mut self, cart: Uuid, fade: bool) {
        let key = self.key(cart);
        let target = StopTarget::Cart(key);
        if fade {
            self.send(EngineCommand::Stop { target, fade: None });
        } else {
            self.send(EngineCommand::Kill { target });
        }
    }

    pub fn stop_page(&mut self, page: usize) {
        self.send(EngineCommand::Stop {
            target: StopTarget::Page(page as u32 + 1),
            fade: None,
        });
    }

    /// Stops everything with each cart's fade (or immediately when `fade` is false).
    pub fn stop_all(&mut self, fade: bool) {
        if fade {
            self.send(EngineCommand::Stop {
                target: StopTarget::All,
                fade: None,
            });
        } else {
            self.send(EngineCommand::Kill {
                target: StopTarget::All,
            });
        }
    }

    fn play_next(&mut self, finished: Uuid) {
        let Some(next) = self
            .project
            .cart(finished)
            .and_then(|c| c.play_next.clone())
        else {
            return;
        };
        let target = if next == "next_cell" {
            self.project.cart_position(finished).and_then(|(p, r, c)| {
                let page = &self.project.pages[p];
                let idx = r as u32 * page.cols as u32 + c as u32 + 1;
                let (nr, nc) = (
                    (idx / page.cols as u32) as u16,
                    (idx % page.cols as u32) as u16,
                );
                page.cell(nr, nc).map(|cell| cell.cart.id)
            })
        } else {
            Uuid::parse_str(&next).ok()
        };
        if let Some(t) = target {
            self.trigger(t, TriggerSource::PlayNext, None);
        }
    }

    // ----- inputs ---------------------------------------------------------

    /// Output peaks per playing cart since the last call (linear, after gain/pan).
    pub fn take_cart_peaks(&self) -> HashMap<Uuid, (f32, f32)> {
        let mut out: HashMap<Uuid, (f32, f32)> = HashMap::new();
        for e in &self.engines {
            e.status().take_voice_peaks(|k, l, r| {
                if let Some(id) = self.by_key.get(&k.0) {
                    let p = out.entry(*id).or_default();
                    p.0 = p.0.max(l);
                    p.1 = p.1.max(r);
                }
            });
        }
        out
    }

    /// Sets a playing cart's gain now (not saved), e.g. while auditioning in Cart Setup.
    pub fn set_live_gain(&mut self, cart: Uuid, gain_db: f32, muted: bool) {
        let key = self.key(cart);
        let gain = if muted { 0.0 } else { db_to_gain(gain_db) };
        self.send(EngineCommand::SetGain { cart: key, gain });
    }

    /// Speed bar: set a playing cart's speed now (not saved), ramping over `ramp_ms`.
    pub fn set_live_speed(&mut self, cart: Uuid, speed: f32, ramp_ms: u32) {
        self.slowed.remove(&cart);
        let key = self.key(cart);
        let ramp_frames = ((ramp_ms as u64 * self.rate() as u64) / 1000).max(1) as u32;
        self.send(EngineCommand::SetSpeed {
            cart: key,
            speed: speed.clamp(0.0, bubble_engine::mixer::MAX_SPEED),
            ramp_frames,
        });
    }

    /// Speed bar "stop like a record player": ramps to 0 over the cart's slow-down time.
    /// The cart keeps its place; playing it again (click, touch or trigger) carries on
    /// at its normal speed.
    pub fn slow_to_stop(&mut self, cart: Uuid) {
        let ms = self.project.cart(cart).map_or(2000, |c| c.slowdown_ms);
        self.set_live_speed(cart, 0.0, ms);
        self.slowed.insert(cart);
    }

    /// A cart that `x0` has brought to 0 % is finished: its voices end, so the next
    /// play (click, touch, hotkey, MIDI) starts again from the In point at real time.
    fn end_halted(&mut self) {
        if self.slowed.is_empty() {
            return;
        }
        if self.engines.is_empty() {
            return;
        }
        let mut moving: std::collections::HashSet<u32> = Default::default();
        let mut present: std::collections::HashSet<u32> = Default::default();
        self.for_each_voice(|v| {
            if v.phase != VoicePhase::Stopping {
                present.insert(v.cart.0);
                if v.speed > 1e-4 {
                    moving.insert(v.cart.0);
                }
            }
        });
        let slowed: Vec<Uuid> = self.slowed.iter().copied().collect();
        for id in slowed {
            let Some(k) = self.keys.get(&id).copied() else {
                self.slowed.remove(&id);
                continue;
            };
            if !present.contains(&k.0) {
                self.slowed.remove(&id);
            } else if !moving.contains(&k.0) {
                self.slowed.remove(&id);
                self.stop_cart(id, false);
            }
        }
    }

    /// Whether `x0` is slowing this cart.
    pub fn is_slowed(&self, cart: Uuid) -> bool {
        self.slowed.contains(&cart) && self.is_playing(cart).is_some()
    }

    /// A cart slowed by `x0` that is played again snaps back to real time (its preset
    /// speed) and carries on from where it is, instead of stopping or restarting.
    fn resume_if_slowed(&mut self, cart: Uuid) -> bool {
        if !self.slowed.remove(&cart) || self.is_playing(cart).is_none() {
            return false;
        }
        let speed = self.project.cart(cart).map_or(1.0, |c| c.speed);
        self.set_live_speed(cart, speed, 10);
        true
    }

    /// Speed bar `x1`: back to normal speed over the cart's speed-up time
    /// (spinning up like a record player; 0 = instant).
    pub fn speed_to_normal(&mut self, cart: Uuid) {
        self.slowed.remove(&cart);
        let ms = self.project.cart(cart).map_or(2000, |c| c.speedup_ms);
        self.set_live_speed(cart, 1.0, ms);
    }

    /// The action a newly learned trigger gets: a key or note behaves like a click
    /// on the cart (toggle in the default mouse mode), so pressing it again stops.
    pub fn default_trigger_action(&self) -> Action {
        match self.project.settings.mouse_mode {
            MouseMode::Toggle => Action::Toggle,
            MouseMode::LeftPlayRightStop => Action::Play,
        }
    }

    /// Changes what an existing binding does.
    pub fn set_binding_action(&mut self, binding: Uuid, action: Action) {
        if self
            .project
            .bindings
            .iter()
            .any(|b| b.id == binding && b.action != action)
        {
            self.checkpoint("binding-action");
            if let Some(b) = self.project.bindings.iter_mut().find(|b| b.id == binding) {
                b.action = action;
            }
            self.mark_dirty();
        }
    }

    /// Stops every hold-to-play cart (e.g. when the window loses keyboard focus and
    /// key releases can no longer be seen).
    pub fn release_holds(&mut self) {
        let holds: Vec<Uuid> = self
            .project
            .carts()
            .filter(|(_, c)| c.cart.hold_to_play)
            .map(|(_, c)| c.cart.id)
            .collect();
        for id in holds {
            if self.is_playing(id).is_some() {
                self.stop_cart(id, true);
            }
        }
    }

    /// Starts learn mode: the next key or MIDI message becomes a binding.
    pub fn start_learn(&mut self, cart: Uuid, action: Action) {
        self.learn = Some(Learn { cart, action });
    }

    pub fn cancel_learn(&mut self) {
        self.learn = None;
    }

    pub fn learning(&self) -> Option<Learn> {
        self.learn
    }

    /// Routes one input event. Returns true if it was consumed (bound or learned).
    pub fn handle_input(&mut self, ev: &InputEvent) -> bool {
        if let Some(learn) = self.learn {
            if let Some(source) = source_from_event(ev) {
                if matches!(&source, BindingSource::Key { key, .. } if key == "Escape") {
                    self.learn = None;
                    return true;
                }
                self.set_binding(learn.cart, source, learn.action);
                self.learn = None;
                return true;
            }
            return false;
        }

        let velocity = match ev {
            InputEvent::Midi { event, .. } => Some(event.value),
            _ => None,
        };
        let source = match ev {
            InputEvent::Key { .. } => TriggerSource::Key,
            InputEvent::Midi { .. } => TriggerSource::Midi,
        };
        let mut actions: Vec<(Binding, Hit)> = Vec::new();
        for b in &self.project.bindings {
            let hit = matches(&b.source, ev);
            if hit != Hit::No {
                actions.push((b.clone(), hit));
            }
        }
        let consumed = !actions.is_empty();

        // Play presses from every matching binding are merged, so one input bound to
        // several carts (in one or many bindings) follows the duplicate policy.
        let mut play_targets: Vec<Uuid> = Vec::new();
        let mut play_vel = None;
        let mut policy = self.project.settings.duplicate_binding_policy;
        let mut overlap_key = String::new();
        for (b, hit) in &actions {
            if *hit == Hit::Press && b.action == Action::Play {
                for t in self.scoped_targets(b) {
                    if !play_targets.contains(&t) {
                        play_targets.push(t);
                    }
                }
                if b.velocity_to_gain {
                    play_vel = velocity;
                }
                if let Some(p) = b.policy {
                    policy = p;
                }
                overlap_key = crate::input::describe(&b.source);
            }
        }
        if !play_targets.is_empty() {
            if policy == DuplicatePolicy::Overlapped && play_targets.len() > 1 {
                if let Some(t) = self.pick_overlapped(&overlap_key, &play_targets) {
                    self.trigger(t, source, play_vel);
                }
            } else {
                for t in play_targets {
                    self.trigger(t, source, play_vel);
                }
            }
        }
        for (b, hit) in actions {
            if !(hit == Hit::Press && b.action == Action::Play) {
                self.run_binding(&b, hit, source, velocity);
            }
        }
        consumed
    }

    fn scoped_targets(&self, b: &Binding) -> Vec<Uuid> {
        let scope = b.scope.unwrap_or(self.project.settings.binding_scope);
        let page = self.current_page;
        b.targets
            .iter()
            .copied()
            .filter(|t| {
                scope == Scope::Global
                    || self.project.cart_position(*t).is_some_and(|p| p.0 == page)
            })
            .collect()
    }

    fn run_binding(&mut self, b: &Binding, hit: Hit, source: TriggerSource, vel: Option<u8>) {
        let page = self.current_page;
        let targets = self.scoped_targets(b);
        if hit == Hit::Release {
            for t in targets {
                if self.project.cart(t).is_some_and(|c| c.hold_to_play) {
                    self.stop_cart(t, true);
                }
            }
            return;
        }
        let vel = if b.velocity_to_gain { vel } else { None };
        match b.action {
            Action::Play => {
                for t in targets {
                    self.trigger(t, source, vel);
                }
            }
            Action::Stop => {
                for t in targets {
                    self.stop_cart(t, true);
                }
            }
            Action::Toggle => {
                for t in targets {
                    if self.resume_if_slowed(t) {
                    } else if self.is_playing(t).is_some() {
                        self.stop_cart(t, true);
                    } else {
                        self.trigger(t, source, vel);
                    }
                }
            }
            Action::Pause => {}
            Action::StopPage => self.stop_page(page),
            Action::StopAll => self.stop_all(true),
            Action::Panic => self.stop_all(false),
            Action::NextPage => {
                let n = self.project.pages.len();
                self.current_page = (self.current_page + 1) % n;
            }
            Action::PrevPage => {
                let n = self.project.pages.len();
                self.current_page = (self.current_page + n - 1) % n;
            }
            Action::GotoPage => {}
        }
    }

    /// Overlapped policy: first target not playing, else least remaining.
    fn pick_overlapped(&mut self, key: &str, targets: &[Uuid]) -> Option<Uuid> {
        let start = *self.overlap_cursor.get(key).unwrap_or(&0) % targets.len();
        let order: Vec<Uuid> = targets[start..]
            .iter()
            .chain(&targets[..start])
            .copied()
            .collect();
        let choice = order
            .iter()
            .copied()
            .find(|t| self.is_playing(*t).is_none())
            .or_else(|| {
                order.iter().copied().min_by(|a, b| {
                    let ra = self.is_playing(*a).unwrap_or(0.0);
                    let rb = self.is_playing(*b).unwrap_or(0.0);
                    ra.total_cmp(&rb)
                })
            })?;
        let idx = targets.iter().position(|t| *t == choice).unwrap_or(0);
        self.overlap_cursor.insert(key.to_string(), idx + 1);
        Some(choice)
    }

    // ----- editing --------------------------------------------------------

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Records the project state before a change. Edits with the same `key`
    /// within one second are merged into one undo step (slider drags, typing).
    fn checkpoint(&mut self, key: &str) {
        if self.batch {
            return;
        }
        let now = std::time::Instant::now();
        if let Some((k, t)) = &self.last_checkpoint
            && k == key
            && now.duration_since(*t) < COALESCE
        {
            self.last_checkpoint = Some((key.to_string(), now));
            return;
        }
        self.last_checkpoint = Some((key.to_string(), now));
        self.undo.push(self.project.clone());
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        let cur = std::mem::replace(&mut self.project, prev);
        self.redo.push(cur);
        self.after_history_change();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let cur = std::mem::replace(&mut self.project, next);
        self.undo.push(cur);
        self.after_history_change();
        true
    }

    fn after_history_change(&mut self) {
        self.last_checkpoint = None;
        self.dirty = true;
        self.current_page = self
            .current_page
            .min(self.project.pages.len().saturating_sub(1));
        self.rebuild_keys();
        let dir = self.project_dir();
        self.assets.sync(&self.project, dir.as_deref());
        // Re-apply live parameters for playing carts.
        let ids: Vec<Uuid> = self.project.carts().map(|(_, c)| c.cart.id).collect();
        for id in ids {
            if let Some(c) = self.project.cart(id).cloned() {
                let key = self.key(id);
                let gain = if c.muted { 0.0 } else { db_to_gain(c.gain_db) };
                self.send(EngineCommand::SetGain { cart: key, gain });
                self.send(EngineCommand::SetLoop {
                    cart: key,
                    looping: c.looping,
                });
            }
        }
        self.apply_master();
    }

    /// Copies a cart (settings + asset reference) for pasting elsewhere.
    pub fn copy_cart(&self, cart: Uuid) -> Option<CartClip> {
        let c = self.project.cart(cart)?.clone();
        let asset = c.asset.and_then(|a| self.project.asset(a)).cloned();
        Some(CartClip { cart: c, asset })
    }

    /// Pastes a copied cart into a cell (replacing what is there). Bindings are
    /// not copied, so the pasted cart never steals the original's triggers.
    pub fn paste_cart(&mut self, page: usize, row: u16, col: u16, clip: &CartClip) -> Option<Uuid> {
        let (rows, cols) = self.project.pages.get(page).map(|p| (p.rows, p.cols))?;
        if row >= rows || col >= cols {
            return None;
        }
        self.checkpoint("paste");
        let mut cart = clip.cart.clone();
        cart.id = Uuid::new_v4();
        cart.play_next = cart.play_next.filter(|n| n == "next_cell");
        if let Some(a) = &clip.asset
            && self.project.asset(a.id).is_none()
        {
            self.project.assets.push(a.clone());
        }
        let p = &mut self.project.pages[page];
        p.cells.retain(|c| !(c.row == row && c.col == col));
        p.cells.push(bubble_project::Cell {
            row,
            col,
            cart: cart.clone(),
        });
        self.key(cart.id);
        self.project.prune_unused_assets();
        let dir = self.project_dir();
        self.assets.sync(&self.project, dir.as_deref());
        self.mark_dirty();
        Some(cart.id)
    }

    /// Copies selected attributes from `from` onto each target cart.
    pub fn paste_attributes(&mut self, from: &Cart, targets: &[Uuid], what: AttrSet) {
        self.checkpoint("paste-attrs");
        self.batch = true;
        for t in targets {
            self.edit_cart(*t, |c| {
                if what.gain {
                    c.gain_db = from.gain_db;
                    c.muted = from.muted;
                }
                if what.pan {
                    c.pan = from.pan;
                }
                if what.fades {
                    c.fade_in = from.fade_in;
                    c.fade_out = from.fade_out;
                }
                if what.colour {
                    c.color = from.color.clone();
                }
                if what.looping {
                    c.looping = from.looping;
                }
                if what.behaviour {
                    c.retrigger = from.retrigger;
                    c.on_play = from.on_play;
                    c.group = from.group;
                    c.hold_to_play = from.hold_to_play;
                }
            });
        }
        self.batch = false;
    }

    /// Edits a cart and applies live parameters (gain, pan, loop) to playing voices.
    pub fn edit_cart(&mut self, cart: Uuid, f: impl FnOnce(&mut Cart)) {
        let Some(c) = self.project.cart(cart) else {
            return;
        };
        let mut after = c.clone();
        f(&mut after);
        if *c == after {
            return;
        }
        let before = c.clone();
        self.checkpoint(&format!("cart:{cart}"));
        *self.project.cart_mut(cart).expect("exists") = after.clone();
        self.mark_dirty();
        let key = self.key(cart);
        if before.gain_db != after.gain_db || before.muted != after.muted {
            let gain = if after.muted {
                0.0
            } else {
                db_to_gain(after.gain_db)
            };
            self.send(EngineCommand::SetGain { cart: key, gain });
        }
        if before.pan != after.pan {
            self.send(EngineCommand::SetPan {
                cart: key,
                pan: after.pan,
            });
        }
        if before.speed != after.speed {
            let ramp = (self.rate() / 20).max(1); // 50 ms
            self.send(EngineCommand::SetSpeed {
                cart: key,
                speed: after.speed,
                ramp_frames: ramp,
            });
        }
        if before.looping != after.looping {
            self.send(EngineCommand::SetLoop {
                cart: key,
                looping: after.looping,
            });
        }
    }

    pub fn edit_settings(&mut self, f: impl FnOnce(&mut bubble_project::ProjectSettings)) {
        self.checkpoint("settings");
        let before = self.project.settings.clone();
        f(&mut self.project.settings);
        if before != self.project.settings {
            self.mark_dirty();
            self.apply_master();
        }
    }

    pub fn rename_project(&mut self, name: &str) {
        self.checkpoint("rename-project");
        if self.project.name != name {
            self.project.name = name.to_string();
            self.mark_dirty();
        }
    }

    /// Assigns an audio file to a grid cell (creating the cart if needed).
    pub fn assign_file(
        &mut self,
        page: usize,
        row: u16,
        col: u16,
        file: &Path,
    ) -> Result<Uuid, ControlError> {
        self.checkpoint("assign");
        if page >= self.project.pages.len() {
            return Err(ControlError::Invalid("no such page".into()));
        }
        let dir = self.project_dir();
        let abs = std::path::absolute(file)?;
        // Reuse an existing asset for the same file.
        let existing = {
            let mut resolver = bubble_project::Resolver::new(dir.as_deref(), &self.project);
            self.project
                .assets
                .iter()
                .find(|a| resolver.resolve(a).path() == Some(abs.as_path()))
                .map(|a| a.id)
        };
        let asset_id = match existing {
            Some(id) => id,
            None => {
                let a = make_asset_ref(dir.as_deref(), &abs)?;
                let id = a.id;
                self.project.assets.push(a);
                id
            }
        };
        let stem = abs
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let cart = self.project.pages[page].ensure_cart(row, col);
        let old_asset = cart.asset.replace(asset_id);
        if cart.label.is_empty() || old_asset.is_some() {
            cart.label = stem;
        }
        let cart_id = cart.id;
        self.key(cart_id);
        self.project.prune_unused_assets();
        self.mark_dirty();
        self.assets.arm(&self.project, dir.as_deref(), asset_id);
        Ok(cart_id)
    }

    /// Assigns files to consecutive cells starting at (row, col), in reading order.
    pub fn assign_files(
        &mut self,
        page: usize,
        row: u16,
        col: u16,
        files: &[PathBuf],
    ) -> Vec<Result<Uuid, String>> {
        let (rows, cols) = match self.project.pages.get(page) {
            Some(p) => (p.rows as u32, p.cols as u32),
            None => return vec![],
        };
        let mut idx = row as u32 * cols + col as u32;
        let mut out = Vec::new();
        for f in files {
            if idx >= rows * cols {
                out.push(Err(format!("{}: page is full", f.display())));
                continue;
            }
            let (r, c) = ((idx / cols) as u16, (idx % cols) as u16);
            out.push(self.assign_file(page, r, c, f).map_err(|e| e.to_string()));
            idx += 1;
        }
        out
    }

    /// Removes the cart in a cell (bindings targeting it are cleaned up).
    pub fn clear_cell(&mut self, page: usize, row: u16, col: u16) {
        self.checkpoint("clear");
        let Some(p) = self.project.pages.get_mut(page) else {
            return;
        };
        let Some(i) = p.cells.iter().position(|c| c.row == row && c.col == col) else {
            return;
        };
        let cart = p.cells.remove(i).cart.id;
        self.stop_cart(cart, true);
        for b in &mut self.project.bindings {
            b.targets.retain(|t| *t != cart);
        }
        self.project
            .bindings
            .retain(|b| !b.targets.is_empty() || !needs_targets(b.action));
        self.project.prune_unused_assets();
        self.mark_dirty();
    }

    /// Moves or swaps the carts of two cells (on any pages).
    pub fn move_cell(&mut self, from: (usize, u16, u16), to: (usize, u16, u16)) {
        self.checkpoint("move");
        if from == to {
            return;
        }
        let take = |p: &mut Page, r: u16, c: u16| {
            p.cells
                .iter()
                .position(|x| x.row == r && x.col == c)
                .map(|i| p.cells.remove(i))
        };
        let Some(a) = self
            .project
            .pages
            .get_mut(from.0)
            .and_then(|p| take(p, from.1, from.2))
        else {
            return;
        };
        let b = self
            .project
            .pages
            .get_mut(to.0)
            .and_then(|p| take(p, to.1, to.2));
        if let Some(mut b) = b {
            b.row = from.1;
            b.col = from.2;
            self.project.pages[from.0].cells.push(b);
        }
        let mut a = a;
        a.row = to.1;
        a.col = to.2;
        self.project.pages[to.0].cells.push(a);
        self.mark_dirty();
    }

    pub fn add_page(&mut self, name: &str, rows: u16, cols: u16) -> usize {
        self.checkpoint("add-page");
        self.project.pages.push(Page::new(name, rows, cols));
        self.mark_dirty();
        self.project.pages.len() - 1
    }

    /// Removes a page and its carts. The last page cannot be removed.
    pub fn remove_page(&mut self, page: usize) -> Result<(), ControlError> {
        if self.project.pages.len() <= 1 {
            return Err(ControlError::Invalid(
                "a project needs at least one page".into(),
            ));
        }
        self.checkpoint("remove-page");
        self.batch = true;
        let cells: Vec<(u16, u16)> = self.project.pages[page]
            .cells
            .iter()
            .map(|c| (c.row, c.col))
            .collect();
        for (r, c) in cells {
            self.clear_cell(page, r, c);
        }
        self.batch = false;
        self.project.pages.remove(page);
        self.current_page = self.current_page.min(self.project.pages.len() - 1);
        self.mark_dirty();
        Ok(())
    }

    /// Appends another project's pages to this one (e.g. an imported SpotOn session):
    /// its audio references, carts and triggers come along; its exclusive groups map
    /// onto this session's by position (G1 → G1), adding groups where needed. One undo
    /// step. Returns the index of the first added page.
    pub fn append_project(&mut self, other: Project) -> usize {
        self.checkpoint("append");
        let first = self.project.pages.len();
        let mut group_map: HashMap<Uuid, Uuid> = HashMap::new();
        for (i, g) in other.groups.iter().enumerate() {
            let id = match self.project.groups.get(i) {
                Some(mine) => mine.id,
                None => {
                    self.project.groups.push(g.clone());
                    g.id
                }
            };
            group_map.insert(g.id, id);
        }
        for a in other.assets {
            if self.project.asset(a.id).is_none() {
                self.project.assets.push(a);
            }
        }
        for mut page in other.pages {
            for cell in &mut page.cells {
                cell.cart.group = cell.cart.group.and_then(|g| group_map.get(&g).copied());
                self.key(cell.cart.id);
            }
            self.project.pages.push(page);
        }
        self.project.bindings.extend(other.bindings);
        let dir = self.project_dir();
        self.assets.sync(&self.project, dir.as_deref());
        self.mark_dirty();
        first
    }

    /// Moves carts from one output to another (`from` = `None`: from any output).
    /// Control buttons are left alone. One undo step; returns how many changed.
    pub fn reassign_outputs(
        &mut self,
        from: Option<&str>,
        to: &str,
        scope: &ReassignScope,
    ) -> usize {
        self.checkpoint("reassign-outputs");
        let mut n = 0;
        for (pi, page) in self.project.pages.iter_mut().enumerate() {
            let cols = page.cols as u32;
            for cell in &mut page.cells {
                let c = &mut cell.cart;
                let number = cell.row as u32 * cols + cell.col as u32 + 1;
                let in_scope = match scope {
                    ReassignScope::All => true,
                    ReassignScope::Page(p) => *p == pi,
                    ReassignScope::Group(g) => c.group == Some(*g),
                    ReassignScope::Buttons { page, first, last } => {
                        *page == pi && (*first..=*last).contains(&number)
                    }
                };
                if in_scope
                    && c.control.is_none()
                    && from.is_none_or(|f| c.output == f)
                    && c.output != to
                {
                    c.output = to.to_string();
                    n += 1;
                }
            }
        }
        if n > 0 {
            self.mark_dirty();
            self.info(format!("Moved {n} cart(s) to Output {to}"));
        }
        n
    }

    /// Copies a page's layout and carts.
    pub fn copy_page(&self, page: usize) -> Option<PageClip> {
        let p = self.project.pages.get(page)?;
        Some(PageClip {
            name: p.name.clone(),
            rows: p.rows,
            cols: p.cols,
            carts: p
                .cells
                .iter()
                .filter_map(|c| Some((c.row, c.col, self.copy_cart(c.cart.id)?)))
                .collect(),
        })
    }

    /// Replaces a page with a copied one: its carts are cleared, the layout becomes
    /// the copy's, and the copied carts are pasted (without their triggers). One
    /// undo step.
    pub fn paste_page(&mut self, page: usize, clip: &PageClip) {
        if page >= self.project.pages.len() {
            return;
        }
        self.checkpoint("paste-page");
        self.batch = true;
        let cells: Vec<(u16, u16)> = self.project.pages[page]
            .cells
            .iter()
            .map(|c| (c.row, c.col))
            .collect();
        for (r, c) in cells {
            self.clear_cell(page, r, c);
        }
        {
            let p = &mut self.project.pages[page];
            p.rows = clip.rows;
            p.cols = clip.cols;
        }
        for (r, c, cart) in &clip.carts {
            self.paste_cart(page, *r, *c, cart);
        }
        self.batch = false;
        self.mark_dirty();
    }

    /// Moves a page to a new position in the page order (current page follows it).
    pub fn move_page(&mut self, from: usize, to: usize) {
        let n = self.project.pages.len();
        if from >= n || to >= n || from == to {
            return;
        }
        self.checkpoint("move-page");
        let current = self.project.pages[self.current_page].id;
        let p = self.project.pages.remove(from);
        self.project.pages.insert(to, p);
        if let Some(i) = self.project.pages.iter().position(|p| p.id == current) {
            self.current_page = i;
        }
        self.mark_dirty();
    }

    pub fn rename_page(&mut self, page: usize, name: &str) {
        if self.project.pages.get(page).is_some_and(|p| p.name != name) {
            self.checkpoint(&format!("rename-page:{page}"));
        }
        if let Some(p) = self.project.pages.get_mut(page)
            && p.name != name
        {
            p.name = name.to_string();
            self.dirty = true;
        }
    }

    /// Resizes a page grid; refuses if carts would fall outside.
    pub fn resize_page(&mut self, page: usize, rows: u16, cols: u16) -> Result<(), ControlError> {
        let max = bubble_project::validate::MAX_GRID;
        if !(1..=max).contains(&rows) || !(1..=max).contains(&cols) {
            return Err(ControlError::Invalid(format!("grid must be 1–{max}")));
        }
        let p = self
            .project
            .pages
            .get(page)
            .ok_or_else(|| ControlError::Invalid("no such page".into()))?;
        if p.cells.iter().any(|c| c.row >= rows || c.col >= cols) {
            return Err(ControlError::Invalid(
                "carts would fall outside the new grid; move or clear them first".into(),
            ));
        }
        if (p.rows, p.cols) != (rows, cols) {
            self.checkpoint(&format!("resize-page:{page}"));
            let p = &mut self.project.pages[page];
            p.rows = rows;
            p.cols = cols;
            self.dirty = true;
        }
        Ok(())
    }

    /// Creates (or finds) an exclusive group by name.
    pub fn ensure_group(&mut self, name: &str) -> Uuid {
        self.checkpoint("group");
        if let Some(g) = self.project.groups.iter().find(|g| g.name == name) {
            return g.id;
        }
        let id = Uuid::new_v4();
        self.project.groups.push(ExclusiveGroup {
            id,
            name: name.into(),
            color: None,
            extra: Extra::new(),
        });
        self.mark_dirty();
        id
    }

    /// Replaces the exclusive group list (rename, add, remove). Carts that referred to
    /// a removed group lose their group.
    pub fn set_groups(&mut self, groups: Vec<ExclusiveGroup>) {
        if groups == self.project.groups {
            return;
        }
        self.checkpoint("groups");
        let ids: std::collections::HashSet<Uuid> = groups.iter().map(|g| g.id).collect();
        self.project.groups = groups;
        for page in &mut self.project.pages {
            for cell in &mut page.cells {
                if cell.cart.group.is_some_and(|g| !ids.contains(&g)) {
                    cell.cart.group = None;
                }
            }
        }
        self.mark_dirty();
    }

    /// Adds an unnamed group (shown as "G<n>") and returns its id.
    pub fn add_group(&mut self) -> Uuid {
        self.checkpoint("group");
        let id = Uuid::new_v4();
        self.project.groups.push(ExclusiveGroup {
            id,
            name: String::new(),
            color: None,
            extra: Extra::new(),
        });
        self.mark_dirty();
        id
    }

    /// Bindings that target `cart`.
    pub fn bindings_for(&self, cart: Uuid) -> Vec<&Binding> {
        self.project
            .bindings
            .iter()
            .filter(|b| b.targets.contains(&cart))
            .collect()
    }

    /// Sets the single binding of this source kind (key or MIDI) for `cart`.
    pub fn set_binding(&mut self, cart: Uuid, source: BindingSource, action: Action) {
        self.checkpoint("binding");
        let same_kind =
            |s: &BindingSource| std::mem::discriminant(s) == std::mem::discriminant(&source);
        self.project
            .bindings
            .retain(|b| !(b.targets == [cart] && same_kind(&b.source)));
        self.project.bindings.push(Binding {
            id: Uuid::new_v4(),
            source,
            action,
            targets: vec![cart],
            policy: None,
            scope: None,
            velocity_to_gain: false,
            extra: Extra::new(),
        });
        self.mark_dirty();
    }

    /// Removes bindings of `cart` (all kinds when `key` is None, else key or MIDI only).
    pub fn clear_bindings(&mut self, cart: Uuid, key: Option<bool>) {
        self.checkpoint("binding-clear");
        let before = self.project.bindings.len();
        self.project.bindings.retain(|b| {
            let is_key = matches!(b.source, BindingSource::Key { .. });
            !(b.targets == [cart] && key.is_none_or(|k| k == is_key))
        });
        if before != self.project.bindings.len() {
            self.mark_dirty();
        }
    }

    /// Other carts bound to the same source (duplicate warning in the UI).
    pub fn duplicates_of(&self, source: &BindingSource, except: Uuid) -> Vec<Uuid> {
        self.project
            .bindings
            .iter()
            .filter(|b| &b.source == source)
            .flat_map(|b| b.targets.iter().copied())
            .filter(|t| *t != except)
            .collect()
    }

    /// Points a missing asset at `file` and auto-relinks others from the same move.
    pub fn relink(&mut self, asset: Uuid, file: &Path) -> Result<usize, ControlError> {
        self.checkpoint("relink");
        let dir = self.project_dir();
        let report = relink(&mut self.project, dir.as_deref(), asset, file)?;
        for id in &report.relinked {
            self.assets.arm(&self.project, dir.as_deref(), *id);
        }
        self.mark_dirty();
        self.info(format!("Relinked {} file(s)", report.relinked.len()));
        Ok(report.relinked.len())
    }

    /// The file each asset resolves to, for assets that can be found.
    pub fn asset_files(&self) -> Vec<(Uuid, PathBuf)> {
        let dir = self.project_dir();
        let mut resolver = bubble_project::Resolver::new(dir.as_deref(), &self.project);
        self.project
            .assets
            .iter()
            .filter_map(|a| resolver.resolve(a).path().map(|p| (a.id, p.to_path_buf())))
            .collect()
    }

    /// Assets whose files live outside the session folder (to be collected into it).
    pub fn external_assets(&self) -> Vec<(Uuid, PathBuf)> {
        let Some(dir) = self.project_dir() else {
            return Vec::new();
        };
        self.asset_files()
            .into_iter()
            .filter(|(_, p)| !bubble_project::session::is_inside(p, &dir))
            .collect()
    }

    /// Points assets at copies of their files (same audio), e.g. after collecting them
    /// into the session's Audio folder. Not an undo step: the audio is unchanged.
    pub fn retarget_assets(&mut self, moves: &[(Uuid, PathBuf)]) {
        let dir = self.project_dir();
        let mut changed = false;
        for (id, file) in moves {
            let Ok(r) = make_asset_ref(dir.as_deref(), file) else {
                continue;
            };
            if let Some(a) = self.project.asset_mut(*id)
                && a.path != r.path
            {
                a.path = r.path;
                a.absolute_hint = r.absolute_hint;
                a.fingerprint = r.fingerprint;
                changed = true;
            }
        }
        if changed {
            for (id, _) in moves {
                self.assets.arm(&self.project, dir.as_deref(), *id);
            }
            self.mark_dirty();
        }
    }

    /// Points assets at converted copies of their files (WAV at another sample
    /// rate). Trims are stored in source frames, so every cart using a converted
    /// asset has its trim rescaled from `from_rate` to `to_rate`.
    pub fn apply_conversions(&mut self, done: &[Conversion]) {
        for d in done {
            if d.from_rate == 0 || d.from_rate == d.to_rate {
                continue;
            }
            let scale = |f: u64| (f as f64 * d.to_rate as f64 / d.from_rate as f64).round() as u64;
            for cart in self
                .project
                .pages
                .iter_mut()
                .flat_map(|p| p.cells.iter_mut())
                .map(|c| &mut c.cart)
                .filter(|c| c.asset == Some(d.asset))
            {
                if let Some(t) = &mut cart.trim {
                    t.in_frames = scale(t.in_frames);
                    t.out_frames = scale(t.out_frames).max(t.in_frames + 1);
                }
            }
            if let Some(a) = self.project.asset_mut(d.asset) {
                a.meta = None;
            }
        }
        let moves: Vec<_> = done.iter().map(|d| (d.asset, d.file.clone())).collect();
        self.retarget_assets(&moves);
        self.mark_dirty();
    }

    /// Saves as a new session file whose assets point at `moves` (copies made in the
    /// new session's Audio folder); unmoved assets are rebased relative to it.
    pub fn save_as_with(
        &mut self,
        path: &Path,
        moves: &[(Uuid, PathBuf)],
    ) -> Result<(), ControlError> {
        let new_dir = std::path::absolute(path.parent().unwrap_or(Path::new(".")))?;
        let old_dir = self.project_dir();
        let mut copy = self.project.clone();
        bubble_project::resolve::rebase_paths(&mut copy, old_dir.as_deref(), &new_dir);
        for (id, file) in moves {
            let r = make_asset_ref(Some(&new_dir), file)?;
            if let Some(a) = copy.asset_mut(*id) {
                a.path = r.path;
                a.absolute_hint = r.absolute_hint;
            }
        }
        pio::save(&mut copy, path)?;
        self.project = copy;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.rearm();
        self.info(format!("Saved {}", path.display()));
        Ok(())
    }

    /// Re-resolves and reloads every asset (e.g. after files were edited).
    pub fn refresh_assets(&mut self) {
        self.rearm();
    }

    // ----- helpers --------------------------------------------------------

    fn rebuild_keys(&mut self) {
        let ids: Vec<Uuid> = self.project.carts().map(|(_, c)| c.cart.id).collect();
        for id in ids {
            self.key(id);
        }
    }

    fn key(&mut self, cart: Uuid) -> CartKey {
        if let Some(k) = self.keys.get(&cart) {
            return *k;
        }
        let k = CartKey(self.next_key);
        self.next_key += 1;
        self.keys.insert(cart, k);
        self.by_key.insert(k.0, cart);
        k
    }

    fn label(&self, cart: Uuid) -> String {
        self.project
            .cart(cart)
            .map(|c| display_label(c, &self.project))
            .unwrap_or_default()
    }

    fn info(&mut self, msg: String) {
        self.log_push(None, String::new(), LogKind::Info(msg));
    }

    fn log_push(&mut self, cart: Option<Uuid>, label: String, kind: LogKind) {
        if self.log.len() >= LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogEntry {
            time: SystemTime::now(),
            cart,
            label,
            kind,
        });
    }
}

fn needs_targets(a: Action) -> bool {
    matches!(
        a,
        Action::Play | Action::Stop | Action::Toggle | Action::Pause
    )
}

/// Label shown on a cart: explicit label, else asset title, else file name.
pub fn display_label(c: &Cart, p: &Project) -> String {
    if !c.label.is_empty() {
        return c.label.clone();
    }
    c.asset
        .and_then(|a| p.asset(a))
        .map(|a| {
            a.meta
                .as_ref()
                .and_then(|m| m.title.clone())
                .unwrap_or_else(|| {
                    a.path
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&a.path)
                        .to_string()
                })
        })
        .unwrap_or_default()
}
