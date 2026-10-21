//! One Bubble at a time.
//!
//! Two copies would compete for the audio devices and write the same session and
//! autosave. The first copy holds a lock on `instance.lock` in the app's data folder
//! (released by the system when the process ends, even after a crash). A second launch
//! leaves a request in `Requests/` (a file to open, or just "come to the front") and
//! exits; the running copy picks requests up within a fraction of a second.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

static LOCK: Mutex<Option<File>> = Mutex::new(None);

fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble").map(|d| d.data_dir().to_path_buf())
}

fn requests_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("Requests"))
}

/// What a launch should do.
pub enum Role {
    /// This is the only copy: carry on.
    First,
    /// Another copy is running and has been asked to show itself (and open `file`).
    Handed,
}

/// Claims the single-instance lock, or hands `file` to the copy already running.
pub fn claim(file: Option<&Path>) -> Role {
    let Some(dir) = data_dir() else {
        return Role::First;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return Role::First;
    }
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("instance.lock"))
    else {
        return Role::First;
    };
    match lock.try_lock() {
        Ok(()) => {
            *LOCK.lock().unwrap_or_else(|e| e.into_inner()) = Some(lock);
            // Requests left over from before this copy started are stale.
            if let Some(r) = requests_dir()
                && let Ok(entries) = std::fs::read_dir(&r)
            {
                for e in entries.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
            Role::First
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            if send_request(file) {
                Role::Handed
            } else {
                Role::First
            }
        }
        // Locking unsupported here (e.g. some network drives): do not block start-up.
        Err(_) => Role::First,
    }
}

/// Gives up the lock, so a relaunch (e.g. with another renderer) can claim it.
pub fn release() {
    if let Some(f) = LOCK.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = f.unlock();
    }
}

fn send_request(file: Option<&Path>) -> bool {
    let Some(dir) = requests_dir() else {
        return false;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let body = file.map_or_else(String::new, |p| {
        std::path::absolute(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
            .to_string()
    });
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!("{}-{stamp}", std::process::id());
    // Written under a temporary name and renamed, so the reader never sees half a file.
    let tmp = dir.join(format!(".{name}"));
    std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, dir.join(name)).is_ok()
}

/// A request from another launch: open this file, or (`None`) just come to the front.
pub type Request = Option<PathBuf>;

/// Watches for requests from later launches (first copy only). `wake` is called when
/// one arrives.
pub fn watch(wake: impl Fn() + Send + 'static) -> Receiver<Request> {
    let (tx, rx) = channel();
    let Some(dir) = requests_dir() else {
        return rx;
    };
    let _ = std::thread::Builder::new()
        .name("bubble-requests".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(300));
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                    {
                        continue;
                    }
                    let body = std::fs::read_to_string(&p).unwrap_or_default();
                    let _ = std::fs::remove_file(&p);
                    let req = (!body.trim().is_empty()).then(|| PathBuf::from(body.trim()));
                    if tx.send(req).is_err() {
                        return;
                    }
                    wake();
                }
            }
        });
    rx
}
