//! What happens when Bubble panics: a crash report is written, unsaved work is
//! autosaved if the session can be reached, and the next launch says so.
//!
//! The regular autosave (every 30 s) still covers a crash in which the session is
//! locked by the code that failed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, TryLockError};

use bubble_control::Controller;

use crate::autosave::{autosave_path, recovery_dir};

/// Name of the file pointing at a report the user has not been told about.
const PENDING: &str = "pending";

fn crash_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble").map(|d| d.data_dir().join("Crashes"))
}

/// Installs the panic hook. Runs on whichever thread panics, including the audio
/// thread, so it takes no lock it might wait on.
pub fn install(ctrl: Arc<Mutex<Controller>>, session_id: u64) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let saved = autosave_now(&ctrl, session_id);
        let report = format!(
            "Bubble {} crashed at {}\n\
             OS: {} {}\n\
             Thread: {}\n\
             {info}\n\
             Unsaved work: {}\n\n\
             Backtrace:\n{}\n",
            env!("CARGO_PKG_VERSION"),
            jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S %Z"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::thread::current().name().unwrap_or("unnamed"),
            saved,
            std::backtrace::Backtrace::force_capture(),
        );
        if let Some(dir) = crash_dir()
            && std::fs::create_dir_all(&dir).is_ok()
        {
            let file = dir.join(format!(
                "Crash {}.txt",
                jiff::Zoned::now().strftime("%Y-%m-%d %H-%M-%S")
            ));
            if std::fs::write(&file, &report).is_ok() {
                let _ = std::fs::write(dir.join(PENDING), file.to_string_lossy().as_bytes());
            }
        }
        previous(info);
    }));
}

/// Writes the session's unsaved changes where the next launch looks for them.
fn autosave_now(ctrl: &Mutex<Controller>, session_id: u64) -> String {
    let c = match ctrl.try_lock() {
        Ok(c) => c,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
        Err(TryLockError::WouldBlock) => {
            return "session busy; the last regular autosave (within 30 s) is offered for recovery"
                .into();
        }
    };
    if !c.is_dirty() {
        return "none".into();
    }
    let json = bubble_project::io::to_json_string(c.project());
    let target = match c.path() {
        Some(p) => Some(autosave_path(p)),
        None => recovery_dir().map(|d| d.join(format!("Untitled-{session_id}.bubble"))),
    };
    drop(c);
    match target {
        Some(t) => match bubble_project::io::write_atomic(&t, json.as_bytes(), false) {
            Ok(()) => format!("autosaved to {}", t.display()),
            Err(e) => format!("autosave failed: {e}"),
        },
        None => "no place to autosave".into(),
    }
}

/// The report of a crash the user has not been told about yet (told once).
pub fn take_unreported() -> Option<PathBuf> {
    let pending = crash_dir()?.join(PENDING);
    let path = std::fs::read_to_string(&pending).ok()?;
    let _ = std::fs::remove_file(&pending);
    let path = PathBuf::from(path.trim());
    path.exists().then_some(path)
}
