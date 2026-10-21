//! Sessions: creating them, opening anything (Bubble sessions, SpotOn sessions and
//! packages), and keeping every audio file inside the session's `Audio` folder.
//!
//! File copies and imports run on a helper thread ([`Job`]) so large files never stall
//! the interface; the result is applied on a later frame.

use std::path::{Path, PathBuf};
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
}

pub struct Job {
    pub label: String,
    /// The cell (page, row, col) the job fills, if any: copying files to assign there.
    pub cell: Option<(usize, u16, u16)>,
    pub rx: Receiver<JobResult>,
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
            move || {
                let mut out = Vec::new();
                let mut errors = Vec::new();
                for f in files {
                    match collect_file(&dir, &f) {
                        Ok(p) => out.push(p),
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                JobResult::Assign(page, row, col, out, errors)
            },
        );
    }

    /// Copies audio that lives outside the session into its Audio folder.
    pub fn collect_external(&mut self) {
        let (dir, external) = {
            let c = self.lock();
            (c.project_dir(), c.external_assets())
        };
        let Some(dir) = dir else { return };
        if external.is_empty() {
            return;
        }
        let n = external.len();
        self.start_job(
            format!(
                "Collecting {n} file{} into the session…",
                if n == 1 { "" } else { "s" }
            ),
            move || {
                let mut moves = Vec::new();
                let mut errors = Vec::new();
                for (id, p) in external {
                    match collect_file(&dir, &p) {
                        Ok(c) => moves.push((id, c)),
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                JobResult::Collected(moves, errors)
            },
        );
    }

    pub fn start_job(&mut self, label: String, f: impl FnOnce() -> JobResult + Send + 'static) {
        self.start_job_at(label, None, f);
    }

    /// As [`Core::start_job`], for a job that fills `cell` (shown on that tile).
    pub fn start_job_at(
        &mut self,
        label: String,
        cell: Option<(usize, u16, u16)>,
        f: impl FnOnce() -> JobResult + Send + 'static,
    ) {
        let (tx, rx) = channel();
        let repaint = self.repaint.clone();
        std::thread::Builder::new()
            .name("bubble-job".into())
            .spawn(move || {
                let _ = tx.send(f());
                repaint();
            })
            .expect("spawn job thread");
        self.job = Some(Job { label, cell, rx });
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
        match ns.then {
            AfterCreate::Empty => intents.push(Intent::CreateSession(file, name)),
            AfterCreate::Assign(p, r, c, files) => {
                intents.push(Intent::CreateSession(file.clone(), name));
                // Copy into the new session once it exists (next frame).
                let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
                let label = "Copying files into the session…".to_string();
                self.start_job_at(label, Some((p, r, c)), move || {
                    let mut out = Vec::new();
                    let mut errors = Vec::new();
                    for f in files {
                        match collect_file(&dir, &f) {
                            Ok(p) => out.push(p),
                            Err(e) => errors.push(e.to_string()),
                        }
                    }
                    JobResult::Assign(p, r, c, out, errors)
                });
            }
            AfterCreate::Import(src) => {
                let out = file.clone();
                self.start_job(format!("Importing {}…", file_name(&src)), move || {
                    let opts = dta_import::ImportOptions {
                        name: Some(name),
                        ..Default::default()
                    };
                    match dta_import::import_file(&src, &out, &opts, false) {
                        Ok(_) => JobResult::Imported(out),
                        Err(e) => {
                            JobResult::Failed(format!("Could not import {}:\n{e}", src.display()))
                        }
                    }
                });
            }
            AfterCreate::SaveAs => {
                let assets = self.lock().asset_files();
                let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
                let n = assets.len();
                self.start_job(
                    format!("Copying {n} audio file(s) to the new session…"),
                    move || {
                        let mut moves = Vec::new();
                        let mut errors = Vec::new();
                        for (id, p) in assets {
                            match collect_file(&dir, &p) {
                                Ok(c) => moves.push((id, c)),
                                Err(e) => errors.push(e.to_string()),
                            }
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
        self.start_job(format!("Importing {}…", file_name(&path)), move || {
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    return JobResult::Failed(format!("Could not read {}:\n{e}", path.display()));
                }
            };
            let name = file_name(&path);
            let opts = dta_import::ImportOptions::default();
            let r = if dta_import::pkg::is_package(&bytes) {
                dta_import::import_package_bytes(&bytes, &name, &dir, &opts, false)
            } else {
                dta_import::import_bytes(&bytes, &name, Some(&dir), &opts, false)
            };
            match r {
                Ok((project, _report)) => JobResult::Merged(Box::new(project)),
                Err(e) => JobResult::Failed(format!("Could not import {}:\n{e}", path.display())),
            }
        });
    }
}
