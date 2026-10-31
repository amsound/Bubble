//! Writes files on its own thread, one at a time and in the order asked. A session,
//! an autosave or the preferences on a slow disk, a network folder or a file antivirus
//! is holding must never hold up a trigger, which runs on the interface thread or
//! waits for the session's lock.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bubble_project::io::{SaveError, write_atomic};

/// How long quitting waits for files still being written.
const FINISH_WAIT: Duration = Duration::from_secs(10);

pub enum WriteJob {
    /// A session, with a backup of the old file. `then_remove` (its autosaves) goes
    /// only once it is safely written.
    Session {
        path: PathBuf,
        bytes: Vec<u8>,
        then_remove: Vec<PathBuf>,
    },
    Autosave {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    Prefs {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    Remove(Vec<PathBuf>),
    Finish(Sender<()>),
}

pub enum WriteDone {
    Session {
        path: PathBuf,
        result: Result<(), String>,
    },
    Autosave(Result<(), String>),
    Prefs(Result<(), String>),
}

fn run(job: WriteJob, done: &Sender<WriteDone>) {
    let text = |path: &PathBuf, e: std::io::Error| {
        SaveError::Io {
            path: path.display().to_string(),
            source: e,
        }
        .to_string()
    };
    let result = match job {
        WriteJob::Session {
            path,
            bytes,
            then_remove,
        } => {
            let result = write_atomic(&path, &bytes, true).map_err(|e| text(&path, e));
            if result.is_ok() {
                for p in then_remove {
                    let _ = std::fs::remove_file(p);
                }
            }
            WriteDone::Session { path, result }
        }
        WriteJob::Autosave { path, bytes } => {
            WriteDone::Autosave(write_atomic(&path, &bytes, false).map_err(|e| text(&path, e)))
        }
        WriteJob::Prefs { path, bytes } => {
            WriteDone::Prefs(write_atomic(&path, &bytes, false).map_err(|e| text(&path, e)))
        }
        WriteJob::Remove(paths) => {
            for p in paths {
                let _ = std::fs::remove_file(p);
            }
            return;
        }
        WriteJob::Finish(ack) => {
            let _ = ack.send(());
            return;
        }
    };
    let _ = done.send(result);
}

pub struct Writer {
    jobs: Mutex<Sender<WriteJob>>,
    done_tx: Sender<WriteDone>,
    done: Receiver<WriteDone>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Writer {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (done_tx, done) = channel();
        let jobs = Mutex::new(spawn(&done_tx, &repaint));
        Writer {
            jobs,
            done_tx,
            done,
            repaint,
        }
    }

    /// Queues a job. If the thread has died, it does the job here: late is better than
    /// lost.
    pub fn submit(&self, job: WriteJob) {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = jobs.send(job) {
            crate::log::info("the file writer stopped; writing on this thread");
            run(e.0, &self.done_tx);
            (self.repaint)();
        }
    }

    /// Files written since the last call.
    pub fn finished(&self) -> Vec<WriteDone> {
        self.done.try_iter().collect()
    }

    /// Waits for everything queued (Bubble is quitting).
    pub fn finish(&self) {
        let (ack, wait) = channel();
        self.submit(WriteJob::Finish(ack));
        if wait.recv_timeout(FINISH_WAIT).is_err() {
            crate::log::info("quitting with a file still being written");
        }
    }
}

fn spawn(done: &Sender<WriteDone>, repaint: &Arc<dyn Fn() + Send + Sync>) -> Sender<WriteJob> {
    let (tx, rx) = channel::<WriteJob>();
    let (done, repaint) = (done.clone(), repaint.clone());
    let _ = std::thread::Builder::new()
        .name("bubble-writer".into())
        .spawn(move || {
            for job in rx {
                run(job, &done);
                repaint();
            }
        });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> Writer {
        Writer::new(Arc::new(|| {}))
    }

    #[test]
    fn a_session_is_written_and_only_then_its_autosave_goes() {
        let dir = tempfile::tempdir().unwrap();
        let (file, auto) = (dir.path().join("a.bubble"), dir.path().join("a.autosave"));
        std::fs::write(&auto, "old").unwrap();
        let w = writer();
        w.submit(WriteJob::Session {
            path: file.clone(),
            bytes: b"new".to_vec(),
            then_remove: vec![auto.clone()],
        });
        w.finish();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        assert!(!auto.exists());
        assert!(matches!(
            w.finished().as_slice(),
            [WriteDone::Session { result: Ok(()), .. }]
        ));
    }

    #[test]
    fn a_failed_save_keeps_its_autosave_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let auto = dir.path().join("a.autosave");
        std::fs::write(&auto, "old").unwrap();
        // A file where the session's folder would have to be.
        let blocker = dir.path().join("not-a-folder");
        std::fs::write(&blocker, "").unwrap();
        let w = writer();
        w.submit(WriteJob::Session {
            path: blocker.join("a.bubble"),
            bytes: b"new".to_vec(),
            then_remove: vec![auto.clone()],
        });
        w.finish();
        assert!(auto.exists());
        assert!(matches!(
            w.finished().as_slice(),
            [WriteDone::Session { result: Err(_), .. }]
        ));
    }

    #[test]
    fn jobs_run_in_the_order_asked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.autosave");
        let w = writer();
        w.submit(WriteJob::Autosave {
            path: file.clone(),
            bytes: b"first".to_vec(),
        });
        w.submit(WriteJob::Remove(vec![file.clone()]));
        w.submit(WriteJob::Autosave {
            path: file.clone(),
            bytes: b"last".to_vec(),
        });
        w.finish();
        assert_eq!(std::fs::read(&file).unwrap(), b"last");
    }
}
