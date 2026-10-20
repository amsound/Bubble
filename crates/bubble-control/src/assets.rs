//! Asset arming: resolves project assets to files and prepares them on a
//! background thread so they are resident before any trigger.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use bubble_engine::SampleBuffer;
use bubble_engine::loader::{SourceInfo, load_for_engine_limited};
use bubble_project::fingerprint::Match;
use bubble_project::{Project, Resolution, Resolver};
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
        buffer: Arc<SampleBuffer>,
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
    pub fn buffer(&self) -> Option<&Arc<SampleBuffer>> {
        match self {
            AssetState::Ready { buffer, .. } => Some(buffer),
            _ => None,
        }
    }
}

struct Job {
    asset: Uuid,
    path: PathBuf,
    rate: u32,
    generation: u64,
    changed: bool,
}

struct Done {
    asset: Uuid,
    path: PathBuf,
    generation: u64,
    changed: bool,
    result: Result<(Arc<SampleBuffer>, SourceInfo), String>,
}

/// Cache of prepared assets plus the worker threads that fill it.
pub struct AssetCache {
    states: HashMap<Uuid, AssetState>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    generation: u64,
    rate: u32,
    /// Bytes held by prepared assets (read by the workers to size their limit).
    used: Arc<AtomicU64>,
    /// Bytes prepared audio may occupy in all.
    limit: u64,
}

const WORKERS: usize = 2;

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
                        let result = load_for_engine_limited(&job.path, job.rate, free)
                            .map_err(|e| e.to_string());
                        if dtx
                            .send(Done {
                                asset: job.asset,
                                path: job.path,
                                generation: job.generation,
                                changed: job.changed,
                                result,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .expect("spawn loader thread");
        }
        AssetCache {
            states: HashMap::new(),
            jobs: jtx,
            done: drx,
            generation: 0,
            rate: 0,
            used,
            limit,
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
            .filter_map(AssetState::buffer)
            .map(|b| b.samples().len() as u64 * 4)
            .sum();
        self.used.store(bytes, Ordering::Relaxed);
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Drops all prepared data and re-arms every asset at `rate`.
    pub fn rearm_all(&mut self, project: &Project, project_dir: Option<&Path>, rate: u32) {
        self.generation += 1;
        self.rate = rate;
        self.states.clear();
        let mut resolver = Resolver::new(project_dir, project);
        for a in &project.assets {
            self.arm_with(&mut resolver, a, rate);
        }
        self.recount();
    }

    /// Arms assets the cache does not know yet and forgets ones no longer in the
    /// project (used after undo/redo and paste).
    pub fn sync(&mut self, project: &Project, project_dir: Option<&Path>) {
        if self.rate == 0 {
            return;
        }
        let ids: std::collections::HashSet<Uuid> = project.assets.iter().map(|a| a.id).collect();
        self.states.retain(|id, _| ids.contains(id));
        let mut resolver = Resolver::new(project_dir, project);
        for a in &project.assets {
            if !self.states.contains_key(&a.id) {
                self.arm_with(&mut resolver, a, self.rate);
            }
        }
        self.recount();
    }

    /// (Re)arms one asset.
    pub fn arm(&mut self, project: &Project, project_dir: Option<&Path>, asset: Uuid) {
        if self.rate == 0 {
            return;
        }
        let Some(a) = project.asset(asset) else {
            self.states.remove(&asset);
            self.recount();
            return;
        };
        let mut resolver = Resolver::new(project_dir, project);
        self.arm_with(&mut resolver, a, self.rate);
        self.recount();
    }

    fn arm_with(&mut self, resolver: &mut Resolver, a: &bubble_project::AssetRef, rate: u32) {
        if rate == 0 {
            return;
        }
        match resolver.resolve(a) {
            Resolution::Found { path, matched, .. } => {
                self.states
                    .insert(a.id, AssetState::Loading { path: path.clone() });
                let _ = self.jobs.send(Job {
                    asset: a.id,
                    path,
                    rate,
                    generation: self.generation,
                    changed: matched == Match::Changed,
                });
            }
            Resolution::Missing { tried } => {
                self.states.insert(a.id, AssetState::Missing { tried });
            }
        }
    }

    /// Applies finished loads. Returns ids whose state changed.
    pub fn poll(&mut self) -> Vec<(Uuid, Option<SourceInfo>)> {
        let mut changed = Vec::new();
        while let Ok(d) = self.done.try_recv() {
            if d.generation != self.generation {
                continue;
            }
            let state = match d.result {
                Ok((buffer, source)) => {
                    changed.push((d.asset, Some(source)));
                    AssetState::Ready {
                        path: d.path,
                        buffer,
                        source,
                        changed: d.changed,
                    }
                }
                Err(message) => {
                    changed.push((d.asset, None));
                    AssetState::Error {
                        path: d.path,
                        message,
                    }
                }
            };
            self.states.insert(d.asset, state);
        }
        if !changed.is_empty() {
            self.recount();
        }
        changed
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
