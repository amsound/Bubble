//! Sessions: creating them, opening anything (Bubble sessions, SpotOn sessions and
//! packages), and keeping every audio file inside the session's `Audio` folder.
//!
//! File copies and imports run on a helper thread ([`Job`]) so large files never stall
//! the interface; the result is applied on a later frame.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, channel};
use std::time::SystemTime;

use bubble_project::session::{collect_file, create_session_folder};
use uuid::Uuid;

use super::{Core, Intent, PendingAction, Snapshot};

/// What happens once a new session folder exists.
#[derive(Clone, Debug)]
pub enum AfterCreate {
    /// An empty session.
    Empty,
    /// Copy these files in and assign them from (page, row, col).
    Assign(usize, u16, u16, Vec<PathBuf>),
    /// Import a SpotOn session (.dta) or package (.pkg) into it.
    Import(PathBuf),
    /// Save the current session (with copies of its audio) as this new one.
    SaveAs,
}

pub struct NewSession {
    pub name: String,
    pub location: PathBuf,
    pub then: AfterCreate,
    pub error: Option<String>,
    /// The new session's Audio files setting: convert what is added to WAV (not used
    /// for Save As, which keeps the session's own settings).
    pub convert_to_wav: bool,
}

pub enum JobResult {
    /// Files copied into the session; assign them from (page, row, col).
    Assign(usize, u16, u16, Vec<PathBuf>, Vec<String>),
    /// Assets copied into the session's Audio folder.
    Collected(Vec<(Uuid, PathBuf)>, Vec<String>),
    /// Audio copied for Save As; save the session to `file` with these moves.
    SavedAs(PathBuf, Vec<(Uuid, PathBuf)>, Vec<String>),
    /// A SpotOn session or package was imported to this session file.
    Imported(PathBuf),
    /// A SpotOn session or package converted for adding to the open session.
    Merged(Box<bubble_project::Project>),
    /// Files converted to WAV (session setting), with errors.
    Converted(Vec<bubble_control::Conversion>, Vec<String>),
    Failed(String),
    /// Stopped when asked.
    Cancelled,
}

/// How a job reports its progress and hears that it should stop. Between files, so a
/// large one finishes first.
#[derive(Clone, Default)]
pub struct JobControl {
    /// Counted in bytes (of a package), not in files.
    pub in_bytes: bool,
    pub import: dta_import::ImportControl,
}

impl JobControl {
    pub fn cancelled(&self) -> bool {
        self.import.cancel.load(Relaxed)
    }

    pub fn set_total(&self, n: u64) {
        self.import.total.store(n, Relaxed);
    }

    pub fn step(&self) {
        self.import.done.fetch_add(1, Relaxed);
    }

    /// (done, total); the total is 0 until it is known.
    pub fn progress(&self) -> (u64, u64) {
        (
            self.import.done.load(Relaxed),
            self.import.total.load(Relaxed),
        )
    }
}

pub struct Job {
    pub label: String,
    /// The cell (page, row, col) the job fills, if any: copying files to assign there.
    pub cell: Option<(usize, u16, u16)>,
    pub rx: Receiver<JobResult>,
    pub control: JobControl,
}

/// `~/Bubble` when no location was chosen yet: the home folder itself is not synced
/// by OneDrive or iCloud by default, unlike Documents and Desktop, and Bubble streams
/// long files from disk, so they must stay on this computer.
fn default_root() -> PathBuf {
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("Bubble"))
        .unwrap_or_else(|| PathBuf::from("Bubble"))
}

/// "Last saved at 09:00", or with the date when not today.
pub fn saved_text(t: SystemTime) -> String {
    let tz = jiff::tz::TimeZone::system();
    let Ok(ts) = jiff::Timestamp::try_from(t) else {
        return "Saved".into();
    };
    let z = ts.to_zoned(tz.clone());
    let today = jiff::Timestamp::now().to_zoned(tz).date();
    if z.date() == today {
        format!("Last saved at {}", z.strftime("%H:%M"))
    } else {
        format!("Last saved {}", z.strftime("%-d %b %Y %H:%M"))
    }
}

/// A path's file name, for messages.
pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

impl Core {
    pub fn session_root(&self) -> PathBuf {
        self.prefs.session_root.clone().unwrap_or_else(default_root)
    }

    /// Opens the New Session dialog.
    pub fn begin_new_session(&mut self, name: String, then: AfterCreate) {
        self.prompts.new_session = Some(NewSession {
            name,
            location: self.session_root(),
            then,
            error: None,
            convert_to_wav: false,
        });
    }

    /// File ▸ Open: a Bubble session opens; a SpotOn session or package is imported
    /// into a new session folder.
    pub fn open_any(&mut self, path: PathBuf, snap: &Snapshot, intents: &mut Vec<Intent>) {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "dta" || ext == "pkg" {
            // With a session open, ask whether to add to it or start a new one.
            if snap.path.is_some() {
                self.prompts.import_choice = Some(path);
            } else {
                self.request(PendingAction::Import(path), snap, intents);
            }
        } else {
            self.request(PendingAction::Open(path), snap, intents);
        }
    }

    /// Assigns audio files, copying them into the session first. Without a session,
    /// asks for one and then continues.
    pub fn assign_into_session(&mut self, page: usize, row: u16, col: u16, files: Vec<PathBuf>) {
        let Some(dir) = self.lock().project_dir() else {
            let name = self.lock().project().name.clone();
            let name = if name == "Untitled" {
                String::new()
            } else {
                name
            };
            self.begin_new_session(name, AfterCreate::Assign(page, row, col, files));
            return;
        };
        let n = files.len();
        self.start_job_at(
            format!(
                "Copying {n} file{} into the session…",
                if n == 1 { "" } else { "s" }
            ),
            Some((page, row, col)),
            move |ctl| {
                let mut out = Vec::new();
                let mut errors = Vec::new();
                ctl.set_total(files.len() as u64);
                for f in files {
                    if ctl.cancelled() {
                        return JobResult::Cancelled;
                    }
                    match collect_file(&dir, &f) {
                        Ok(p) => out.push(p),
                        Err(e) => errors.push(e.to_string()),
                    }
                    ctl.step();
                }
                JobResult::Assign(page, row, col, out, errors)
            },
        );
    }

    /// Copies audio that lives outside the session into its Audio folder.
    pub fn collect_external(&mut self) {
        let Some(dir) = self.lock().project_dir() else {
            return;
        };
        let external: Vec<_> = self
            .asset_files()
            .into_iter()
            .filter(|(_, p)| !bubble_project::session::is_inside(p, &dir))
            .collect();
        if external.is_empty() {
            return;
        }
        let n = external.len();
        self.start_job(
            format!(
                "Collecting {n} file{} into the session…",
                if n == 1 { "" } else { "s" }
            ),
            move |ctl| {
                let mut moves = Vec::new();
                let mut errors = Vec::new();
                ctl.set_total(external.len() as u64);
                for (id, p) in external {
                    // Those copied so far are kept; the session still works without the rest.
                    if ctl.cancelled() {
                        break;
                    }
                    match collect_file(&dir, &p) {
                        Ok(c) => moves.push((id, c)),
                        Err(e) => errors.push(e.to_string()),
                    }
                    ctl.step();
                }
                JobResult::Collected(moves, errors)
            },
        );
    }

    pub fn start_job(
        &mut self,
        label: String,
        f: impl FnOnce(&JobControl) -> JobResult + Send + 'static,
    ) {
        self.start_job_at(label, None, f);
    }

    /// Asks the running job to stop.
    pub fn cancel_job(&mut self) {
        if let Some(j) = &self.job {
            crate::log::info(&format!("cancelling: {}", j.label));
            j.control.import.cancel.store(true, Relaxed);
        }
    }

    /// As [`Core::start_job`], for a job that fills `cell` (shown on that tile).
    pub fn start_job_at(
        &mut self,
        label: String,
        cell: Option<(usize, u16, u16)>,
        f: impl FnOnce(&JobControl) -> JobResult + Send + 'static,
    ) {
        let control = JobControl::default();
        self.start_job_with(label, cell, control, f);
    }

    fn start_job_with(
        &mut self,
        label: String,
        cell: Option<(usize, u16, u16)>,
        control: JobControl,
        f: impl FnOnce(&JobControl) -> JobResult + Send + 'static,
    ) {
        let (tx, rx) = channel();
        let theirs = control.clone();
        let repaint = self.repaint.clone();
        std::thread::Builder::new()
            .name("bubble-job".into())
            .spawn(move || {
                let _ = tx.send(f(&theirs));
                repaint();
            })
            .expect("spawn job thread");
        self.job = Some(Job {
            label,
            cell,
            rx,
            control,
        });
    }

    /// Applies a finished job.
    pub fn poll_job(&mut self, intents: &mut Vec<Intent>) {
        let Some(job) = &self.job else { return };
        let Ok(result) = job.rx.try_recv() else {
            return;
        };
        self.job = None;
        let report = |errors: Vec<String>, this: &mut Core| {
            if !errors.is_empty() {
                this.prompts.error = Some(format!(
                    "Some files could not be copied:\n{}",
                    errors.join("\n")
                ));
            }
        };
        match result {
            JobResult::Cancelled => crate::log::info("cancelled"),
            JobResult::Assign(p, r, c, files, errors) => {
                if !files.is_empty() {
                    intents.push(Intent::AssignFiles(p, r, c, files));
                    self.want_wav_check(false);
                }
                report(errors, self);
            }
            JobResult::Collected(moves, errors) => {
                intents.push(Intent::Retarget(moves));
                self.want_wav_check(false);
                report(errors, self);
            }
            JobResult::SavedAs(file, moves, errors) => {
                intents.push(Intent::SaveAsWith(file, moves));
                report(errors, self);
            }
            JobResult::Imported(file) => {
                self.start_open(file, false);
            }
            JobResult::Merged(project) => {
                intents.push(Intent::Append(*project));
                self.want_collect = true;
                self.want_wav_check(false);
            }
            JobResult::Converted(done, errors) => {
                if !done.is_empty() {
                    intents.push(Intent::Converted(done));
                }
                if !errors.is_empty() {
                    self.prompts.error = Some(format!(
                        "Some files could not be converted to WAV:\n{}",
                        errors.join("\n")
                    ));
                }
            }
            JobResult::Failed(e) => self.prompts.error = Some(e),
        }
    }

    /// The chosen folder from the dialog's "Choose…".
    pub fn set_session_location(&mut self, dir: PathBuf) {
        self.prefs.session_root = Some(dir.clone());
        self.save_prefs();
        if let Some(ns) = &mut self.prompts.new_session {
            ns.location = dir;
        }
    }

    pub fn create_session(&mut self, intents: &mut Vec<Intent>) {
        let Some(ns) = self.prompts.new_session.take() else {
            return;
        };
        let file = match create_session_folder(&ns.location, &ns.name) {
            Ok(f) => f,
            Err(e) => {
                self.prompts.new_session = Some(NewSession {
                    error: Some(e.to_string()),
                    ..ns
                });
                return;
            }
        };
        let name = bubble_project::session::clean_name(&ns.name);
        let wav = ns.convert_to_wav;
        match ns.then {
            AfterCreate::Empty => intents.push(Intent::CreateSession(file, name, wav)),
            AfterCreate::Assign(p, r, c, files) => {
                intents.push(Intent::CreateSession(file.clone(), name, wav));
                // Copy into the new session once it exists (next frame).
                let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
                let label = "Copying files into the session…".to_string();
                self.start_job_at(label, Some((p, r, c)), move |ctl| {
                    let mut out = Vec::new();
                    let mut errors = Vec::new();
                    ctl.set_total(files.len() as u64);
                    for f in files {
                        if ctl.cancelled() {
                            return JobResult::Cancelled;
                        }
                        match collect_file(&dir, &f) {
                            Ok(p) => out.push(p),
                            Err(e) => errors.push(e.to_string()),
                        }
                        ctl.step();
                    }
                    JobResult::Assign(p, r, c, out, errors)
                });
            }
            AfterCreate::Import(src) => {
                let out = file.clone();
                let control = JobControl {
                    in_bytes: true,
                    ..Default::default()
                };
                let label = format!("Importing {}…", file_name(&src));
                self.start_job_with(label, None, control, move |ctl| {
                    let opts = dta_import::ImportOptions {
                        name: Some(name),
                        control: Some(ctl.import.clone()),
                        ..Default::default()
                    };
                    match dta_import::import_file(&src, &out, &opts, false) {
                        // The session's folder was made for this import (it is refused
                        // unless new or empty), so nothing else goes with it.
                        Err(dta_import::ImportError::Cancelled) => {
                            if let Some(dir) = out.parent() {
                                let _ = std::fs::remove_dir_all(dir);
                            }
                            JobResult::Cancelled
                        }
                        Ok(_) if wav => {
                            // The Audio files choice made with the new session.
                            let set = bubble_project::io::load(&out)
                                .map_err(|e| e.to_string())
                                .and_then(|mut p| {
                                    p.settings.convert_to_wav = true;
                                    bubble_project::io::save(&mut p, &out)
                                        .map_err(|e| e.to_string())
                                });
                            match set {
                                Ok(()) => JobResult::Imported(out),
                                Err(e) => JobResult::Failed(format!(
                                    "Imported {}, but could not set it to convert to WAV:\n{e}",
                                    src.display()
                                )),
                            }
                        }
                        Ok(_) => JobResult::Imported(out),
                        Err(e) => {
                            JobResult::Failed(format!("Could not import {}:\n{e}", src.display()))
                        }
                    }
                });
            }
            AfterCreate::SaveAs => {
                let assets = self.asset_files();
                let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
                let n = assets.len();
                self.start_job(
                    format!("Copying {n} audio file(s) to the new session…"),
                    move |ctl| {
                        let mut moves = Vec::new();
                        let mut errors = Vec::new();
                        ctl.set_total(assets.len() as u64);
                        for (id, p) in assets {
                            if ctl.cancelled() {
                                return JobResult::Cancelled;
                            }
                            match collect_file(&dir, &p) {
                                Ok(c) => moves.push((id, c)),
                                Err(e) => errors.push(e.to_string()),
                            }
                            ctl.step();
                        }
                        JobResult::SavedAs(file, moves, errors)
                    },
                );
            }
        }
    }

    /// "Add to Current Session" for a SpotOn session or package: import it on a helper
    /// thread and append its pages.
    pub fn import_into_current(&mut self, path: PathBuf) {
        let Some(dir) = self.lock().project_dir() else {
            return;
        };
        let control = JobControl {
            in_bytes: true,
            ..Default::default()
        };
        let label = format!("Importing {}…", file_name(&path));
        self.start_job_with(label, None, control, move |ctl| {
            let name = file_name(&path);
            let opts = dta_import::ImportOptions {
                control: Some(ctl.import.clone()),
                ..Default::default()
            };
            match dta_import::import_source(&path, &name, &dir, &opts, false) {
                Ok((project, _report)) => JobResult::Merged(Box::new(project)),
                Err(dta_import::ImportError::Cancelled) => JobResult::Cancelled,
                Err(e) => JobResult::Failed(format!("Could not import {}:\n{e}", path.display())),
            }
        });
    }
}
