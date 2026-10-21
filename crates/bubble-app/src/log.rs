//! A small text log of start-up and notable events, so a launch that never shows a
//! window (no console on Windows) can still be diagnosed. One file in the app's data
//! folder, `Logs/bubble.log`; started afresh when it passes 1 MB.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Log {
    file: Option<std::fs::File>,
    start: Instant,
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();

/// Folder holding the log (and crash reports, one level up in `Crashes`).
pub fn dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble").map(|d| d.data_dir().join("Logs"))
}

/// Opens the log; call once, first thing in `main`.
pub fn init() {
    let file = dir().and_then(|d| {
        std::fs::create_dir_all(&d).ok()?;
        let path = d.join("bubble.log");
        let big = std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000);
        std::fs::OpenOptions::new()
            .create(true)
            .append(!big)
            .write(true)
            .truncate(big)
            .open(path)
            .ok()
    });
    let _ = LOG.set(Mutex::new(Log {
        file,
        start: Instant::now(),
    }));
    info(&format!(
        "---- Bubble {} starting ({} {}) at {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S")
    ));
}

/// Appends a line, stamped with the seconds since start. Never fails.
pub fn info(msg: &str) {
    if let Some(m) = LOG.get()
        && let Ok(mut log) = m.lock()
    {
        let t = log.start.elapsed().as_secs_f64();
        if let Some(f) = &mut log.file {
            let _ = writeln!(f, "[{t:8.3}] {msg}");
            let _ = f.flush();
        }
    }
}
