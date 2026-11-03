//! Asset arming: resolves project assets to files and prepares them on a
//! background thread so they are ready before any trigger. Short files are held
//! whole in memory; long ones keep a pre-roll head at each cart's In point and
//! stream the rest while playing (decision D-37).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use bubble_engine::loader::{PrepareOptions, SourceInfo, engine_frame, prepare, scan_overview};
use bubble_engine::markers::{self, FileSections};
use bubble_engine::{Overview, Playable};
use bubble_project::fingerprint::Match;
use bubble_project::{Project, Resolution, Resolver};

/// Where each asset's file is. Looks at the disk, so made off the session's lock.
pub type Resolved = HashMap<Uuid, Resolution>;

/// Looks up every asset of a project, a copy of it, on any thread.
pub fn resolve_all(project: &Project, project_dir: Option<&Path>) -> Resolved {
    let mut resolver = Resolver::new(project_dir, project);
    project
        .assets
        .iter()
        .map(|a| (a.id, resolver.resolve(a)))
        .collect()
}
use crossbeam_channel::{Receiver, Sender, unbounded};
use uuid::Uuid;

/// Readiness of one asset.
#[derive(Clone, Debug)]
pub enum AssetState {
    Loading {
        path: PathBuf,
    },
    Ready {
        path: PathBuf,
        audio: Arc<Playable>,
        source: SourceInfo,
        /// The file differs from the fingerprint stored in the project.
        changed: bool,
    },
    Missing {
        tried: Vec<PathBuf>,
    },
    Error {
        path: PathBuf,
        message: String,
    },
}

impl AssetState {
    pub fn audio(&self) -> Option<&Arc<Playable>> {
        match self {
            AssetState::Ready { audio, .. } => Some(audio),
            _ => None,
        }
    }
}

/// The asset edited in Cart Setup, its whole-file copy once loaded, and where the load
/// reports.
type Editing = (
    Uuid,
    Option<Arc<Playable>>,
    std::sync::mpsc::Receiver<Option<Arc<Playable>>>,
);

struct Job {
    asset: Uuid,
    path: PathBuf,
    rate: u32,
    /// In points of the carts playing this asset (source frames).
    starts: Vec<u64>,
    generation: u64,
    changed: bool,
}

struct Done {
    asset: Uuid,
    path: PathBuf,
    generation: u64,
    changed: bool,
    result: Result<(Arc<Playable>, SourceInfo), String>,
    /// Sections marked in the file.
    markers: Option<FileSections>,
}

struct OverviewDone {
    asset: Uuid,
    generation: u64,
    result: Result<Overview, String>,
}

enum OverviewState {
    Pending,
    Ready(Arc<Overview>),
    Failed,
}

/// Cache of prepared assets plus the worker threads that fill it.
pub struct AssetCache {
    last_loaded: Option<PathBuf>,
    states: HashMap<Uuid, AssetState>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    generation: u64,
    rate: u32,
    /// Bytes held by prepared assets (read by the workers to size their limit).
    used: Arc<AtomicU64>,
    /// Bytes prepared audio may occupy in all.
    limit: u64,
    /// Assets being prepared again for new In points (still playable meanwhile).
    reheading: HashSet<Uuid>,
    overviews: HashMap<Uuid, OverviewState>,
    /// An In point being edited (not saved yet) per asset, which gets a head too.
    draft_starts: HashMap<Uuid, u64>,
    overview_jobs: Sender<(Uuid, PathBuf, u64)>,
    overview_done: Receiver<OverviewDone>,
    editing: Option<Editing>,
}

const WORKERS: usize = 2;

/// In points (source frames) of the carts that play each asset.
fn cart_starts(project: &Project) -> HashMap<Uuid, Vec<u64>> {
    let mut m: HashMap<Uuid, Vec<u64>> = HashMap::new();
    for (_, cell) in project.carts() {
        let c = &cell.cart;
        let Some(a) = c.asset else { continue };
        // The In point, and each section's: Main's In comes round on every loop and
        // the Outro may start at any moment.
        let sections = c.sections.into_iter().flat_map(|s| {
            [s.intro.map(|x| x.in_frames), Some(s.main.in_frames), s.outro.map(|x| x.in_frames)]
        });
        for at in c.trim.map(|t| t.in_frames).into_iter().chain(sections.flatten()) {
            let v = m.entry(a).or_default();
            if at > 0 && !v.contains(&at) {
                v.push(at);
            }
        }
    }
    m
}

impl AssetCache {
    pub fn new() -> Self {
        Self::with_budget(crate::memory::audio_budget())
    }

    /// A cache whose prepared audio may occupy at most `limit` bytes; files that
    /// would not fit are refused with an error rather than exhausting memory.
    pub fn with_budget(limit: u64) -> Self {
        let used = Arc::new(AtomicU64::new(0));
        let (jtx, jrx) = unbounded::<Job>();
        let (dtx, drx) = unbounded::<Done>();
        for i in 0..WORKERS {
            let jrx = jrx.clone();
            let dtx = dtx.clone();
            let used = used.clone();
            thread::Builder::new()
                .name(format!("bubble-loader-{i}"))
                .spawn(move || {
                    for job in jrx {
                        let free = limit.saturating_sub(used.load(Ordering::Relaxed));
                        // A decoder that panics on a damaged file fails that file, and this
                        // thread lives on for the rest.
                        let result = std::panic::catch_unwind(|| {
                            prepare(
                                &job.path,
                                job.rate,
                                &job.starts,
                                PrepareOptions::default(),
                                free,
                            )
                            .map_err(|e| e.to_string())
                        })
                        .unwrap_or_else(|_| Err("the file could not be decoded".to_string()));
                        let markers = std::panic::catch_unwind(|| markers::read(&job.path))
                            .ok()
                            .flatten();
                        if dtx
                            .send(Done {
                                asset: job.asset,
                                path: job.path,
                                generation: job.generation,
                                changed: job.changed,
                                result,
                                markers,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .expect("spawn loader thread");
        }
        // Overviews read whole files, so they get their own thread and never hold up
        // arming.
        let (otx, orx) = unbounded::<(Uuid, PathBuf, u64)>();
        let (odtx, odrx) = unbounded::<OverviewDone>();
        thread::Builder::new()
            .name("bubble-overview".into())
            .spawn(move || {
                for (asset, path, generation) in orx {
                    let result = std::panic::catch_unwind(|| {
                        scan_overview(&path).map_err(|e| e.to_string())
                    })
                    .unwrap_or_else(|_| Err("the file could not be decoded".to_string()));
                    if odtx
                        .send(OverviewDone {
                            asset,
                            generation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn overview thread");
        AssetCache {
            states: HashMap::new(),
            jobs: jtx,
            done: drx,
            last_loaded: None,
            generation: 0,
            rate: 0,
            used,
            limit,
            reheading: HashSet::new(),
            overviews: HashMap::new(),
            draft_starts: HashMap::new(),
            overview_jobs: otx,
            overview_done: odrx,
            editing: None,
        }
    }

    /// Keeps the whole of `asset` in memory (a long file being edited in Cart Setup),
    /// loaded on a thread of its own, so its auditions and scrubbing never wait for
    /// the disk; `None` lets it go. Cheap when nothing changes; call regularly.
    pub fn edit_in_memory(&mut self, asset: Option<Uuid>) {
        if self.editing.as_ref().map(|e| e.0) == asset {
            // Take a finished load.
            if let Some((_, slot @ None, rx)) = &mut self.editing
                && let Ok(audio) = rx.try_recv()
            {
                *slot = audio;
            }
            return;
        }
        self.editing = None;
        let Some(asset) = asset else { return };
        let Some(AssetState::Ready { path, audio, .. }) = self.states.get(&asset) else {
            return;
        };
        if !audio.is_streamed() {
            return;
        }
        let (path, rate) = (path.clone(), self.rate);
        let free = self.limit.saturating_sub(self.used.load(Ordering::Relaxed));
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = thread::Builder::new()
            .name("bubble-edit-load".into())
            .spawn(move || {
                let r = std::panic::catch_unwind(|| {
                    bubble_engine::loader::load_for_engine_limited(&path, rate, free)
                });
                let audio = match r {
                    Ok(Ok((buf, _))) => Arc::try_unwrap(buf).ok().map(Playable::resident),
                    _ => None,
                };
                let _ = tx.send(audio);
            });
        if spawned.is_ok() {
            self.editing = Some((asset, None, rx));
        }
    }

    /// The whole-file copy of the asset being edited, once loaded.
    pub fn editing_audio(&self, asset: Uuid) -> Option<Arc<Playable>> {
        match &self.editing {
            Some((a, Some(audio), _)) if *a == asset => Some(audio.clone()),
            _ => None,
        }
    }

    /// (bytes held by prepared audio, bytes allowed).
    pub fn memory(&self) -> (u64, u64) {
        (self.used.load(Ordering::Relaxed), self.limit)
    }

    /// Recomputes the bytes held by prepared assets.
    fn recount(&self) {
        let bytes: u64 = self
            .states
            .values()
            .filter_map(AssetState::audio)
            .map(|b| b.bytes())
            .sum();
        self.used.store(bytes, Ordering::Relaxed);
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The last file to finish loading, to show.
    pub fn last_loaded(&self) -> Option<&Path> {
        self.last_loaded.as_deref()
    }

    /// Where an asset's file was found when it was armed, without looking at the disk.
    pub fn found_at(&self, asset: Uuid) -> Option<&Path> {
        match self.states.get(&asset)? {
            AssetState::Ready { path, .. }
            | AssetState::Loading { path }
            | AssetState::Error { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Drops all prepared data and re-arms every asset at `rate`.
    /// `known` holds files already looked up off the session's lock; any others are
    /// looked up here.
    pub fn rearm_all(
        &mut self,
        project: &Project,
        project_dir: Option<&Path>,
        rate: u32,
        known: Option<&Resolved>,
    ) {
        self.generation += 1;
        self.rate = rate;
        self.states.clear();
        self.reheading.clear();
        self.overviews.clear();
        self.draft_starts.clear();
        let starts = self.starts(project);
        let mut resolver = Resolver::new(project_dir, project);
        for a in &project.assets {
            match known.and_then(|k| k.get(&a.id)) {
                Some(found) => self.arm_resolved(a, found.clone(), rate, &starts),
                None => self.arm_with(&mut resolver, a, rate, &starts),
            }
        }
        self.recount();
    }

    /// Arms assets the cache does not know yet and forgets ones no longer in the
    /// project (used after undo/redo and paste).
    pub fn sync(&mut self, project: &Project, project_dir: Option<&Path>) {
        if self.rate == 0 {
            return;
        }
        let ids: HashSet<Uuid> = project.assets.iter().map(|a| a.id).collect();
        self.states.retain(|id, _| ids.contains(id));
        self.overviews.retain(|id, _| ids.contains(id));
        let starts = self.starts(project);
        let mut resolver = Resolver::new(project_dir, project);
        for a in &project.assets {
            if !self.states.contains_key(&a.id) {
                self.arm_with(&mut resolver, a, self.rate, &starts);
            }
        }
        self.recount();
    }

    /// (Re)arms one asset.
    pub fn arm(&mut self, project: &Project, project_dir: Option<&Path>, asset: Uuid) {
        if self.rate == 0 {
            return;
        }
        self.overviews.remove(&asset);
        let Some(a) = project.asset(asset) else {
            self.states.remove(&asset);
            self.recount();
            return;
        };
        let starts = self.starts(project);
        let mut resolver = Resolver::new(project_dir, project);
        self.arm_with(&mut resolver, a, self.rate, &starts);
        self.recount();
    }

    fn arm_with(
        &mut self,
        resolver: &mut Resolver,
        a: &bubble_project::AssetRef,
        rate: u32,
        starts: &HashMap<Uuid, Vec<u64>>,
    ) {
        if rate == 0 {
            return;
        }
        let found = resolver.resolve(a);
        self.arm_resolved(a, found, rate, starts);
    }

    fn arm_resolved(
        &mut self,
        a: &bubble_project::AssetRef,
        found: Resolution,
        rate: u32,
        starts: &HashMap<Uuid, Vec<u64>>,
    ) {
        if rate == 0 {
            return;
        }
        self.reheading.remove(&a.id);
        match found {
            Resolution::Found { path, matched, .. } => {
                self.states
                    .insert(a.id, AssetState::Loading { path: path.clone() });
                let _ = self.jobs.send(Job {
                    asset: a.id,
                    path,
                    rate,
                    starts: starts.get(&a.id).cloned().unwrap_or_default(),
                    generation: self.generation,
                    changed: matched == Match::Changed,
                });
            }
            Resolution::Missing { tried } => {
                self.states.insert(a.id, AssetState::Missing { tried });
            }
        }
    }

    /// In points that get a head: the carts' and the one being edited.
    fn starts(&self, project: &Project) -> HashMap<Uuid, Vec<u64>> {
        let mut m = cart_starts(project);
        for (asset, s) in &self.draft_starts {
            let v = m.entry(*asset).or_default();
            if !v.contains(s) {
                v.push(*s);
            }
        }
        m
    }

    /// Also keeps a head at `start` (source frames) of `asset`: an In point being
    /// edited in Cart Setup, so auditioning it starts at once. Only the latest such
    /// point per asset is kept; [`Self::refresh_heads`] prepares it.
    pub fn want_head(&mut self, asset: Uuid, start: u64) {
        if start > 0 {
            self.draft_starts.insert(asset, start);
        }
    }

    /// Prepares streamed assets again where a cart's In point has no pre-roll head
    /// (after a trim edit), so every saved In starts instantly. The asset stays
    /// playable meanwhile. Cheap when nothing changed; call regularly.
    pub fn refresh_heads(&mut self, project: &Project) {
        if self.rate == 0 {
            return;
        }
        for (asset, starts) in self.starts(project) {
            if self.reheading.contains(&asset) {
                continue;
            }
            let Some(AssetState::Ready {
                path,
                audio,
                source,
                changed,
            }) = self.states.get(&asset)
            else {
                continue;
            };
            if !audio.is_streamed() {
                continue;
            }
            let missing = starts.iter().any(|s| {
                let at = engine_frame(*s, source.sample_rate, self.rate);
                at < audio.frames() && !audio.heads().iter().any(|h| h.start == at)
            });
            if missing {
                self.reheading.insert(asset);
                let _ = self.jobs.send(Job {
                    asset,
                    path: path.clone(),
                    rate: self.rate,
                    starts,
                    generation: self.generation,
                    changed: *changed,
                });
            }
        }
    }

    /// Applies finished loads. Returns ids whose state changed, with the file's
    /// details and the sections marked in it.
    pub fn poll(&mut self) -> Vec<(Uuid, Option<SourceInfo>, Option<FileSections>)> {
        let mut changed = Vec::new();
        while let Ok(d) = self.done.try_recv() {
            if d.generation != self.generation {
                continue;
            }
            self.last_loaded = Some(d.path.clone());
            if self.reheading.remove(&d.asset) {
                // New heads for an asset that already plays: keep the old audio if
                // this attempt failed.
                if let (Ok((audio, _)), Some(AssetState::Ready { audio: cur, .. })) =
                    (d.result, self.states.get_mut(&d.asset))
                {
                    *cur = audio;
                }
                continue;
            }
            let state = match d.result {
                Ok((audio, source)) => {
                    changed.push((d.asset, Some(source), d.markers));
                    AssetState::Ready {
                        path: d.path,
                        audio,
                        source,
                        changed: d.changed,
                    }
                }
                Err(message) => {
                    changed.push((d.asset, None, None));
                    AssetState::Error {
                        path: d.path,
                        message,
                    }
                }
            };
            self.states.insert(d.asset, state);
        }
        while let Ok(d) = self.overview_done.try_recv() {
            if d.generation != self.generation
                || !matches!(self.overviews.get(&d.asset), Some(OverviewState::Pending))
            {
                continue;
            }
            let state = match d.result {
                Ok(o) => OverviewState::Ready(Arc::new(o)),
                Err(_) => OverviewState::Failed,
            };
            self.overviews.insert(d.asset, state);
        }
        self.recount();
        changed
    }

    /// The waveform overview of a ready asset, computed on a background thread the
    /// first time it is asked for (`None` until then, or if the file cannot be read).
    pub fn overview(&mut self, asset: Uuid) -> Option<Arc<Overview>> {
        match self.overviews.get(&asset) {
            Some(OverviewState::Ready(o)) => return Some(o.clone()),
            Some(_) => return None,
            None => {}
        }
        let Some(AssetState::Ready { path, .. }) = self.states.get(&asset) else {
            return None;
        };
        let _ = self
            .overview_jobs
            .send((asset, path.clone(), self.generation));
        self.overviews.insert(asset, OverviewState::Pending);
        None
    }

    pub fn state(&self, asset: Uuid) -> Option<&AssetState> {
        self.states.get(&asset)
    }

    pub fn pending(&self) -> usize {
        self.states
            .values()
            .filter(|s| matches!(s, AssetState::Loading { .. }))
            .count()
    }

    pub fn missing(&self) -> Vec<Uuid> {
        self.states
            .iter()
            .filter(|(_, s)| matches!(s, AssetState::Missing { .. }))
            .map(|(id, _)| *id)
            .collect()
    }
}

impl Default for AssetCache {
    fn default() -> Self {
        Self::new()
    }
}
