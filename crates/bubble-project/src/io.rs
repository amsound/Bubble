//! Reading and writing project files.
//!
//! * Parsing is strict JSON → migration by `format_version` → typed model →
//!   semantic validation. Any failure rejects the whole file.
//! * Saving writes a temp file in the target directory, fsyncs it, keeps the
//!   previous version as `<file>.bak`, then atomically renames over the target.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{CURRENT_FORMAT_VERSION, FORMAT_NAME, Project, app_version};
use crate::validate::{Issue, validate};

pub const EXTENSION: &str = "bubble";
pub const MAX_PROJECT_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("project file is larger than {} MB", MAX_PROJECT_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("not valid JSON (line {line}, column {column}): {message}")]
    Syntax {
        line: usize,
        column: usize,
        message: String,
    },
    #[error("not a Bubble project")]
    NotAProject,
    #[error(
        "project format version {0} is newer than this app supports ({CURRENT_FORMAT_VERSION})"
    )]
    TooNew(u32),
    #[error("project structure invalid: {0}")]
    Shape(String),
    #[error("project has {} problem(s): {}", .0.len(), summarize(.0))]
    Invalid(Vec<Issue>),
}

fn summarize(issues: &[Issue]) -> String {
    let mut s = issues
        .iter()
        .take(5)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    if issues.len() > 5 {
        s.push_str("; …");
    }
    s
}

/// Parses, migrates and validates a project from JSON text.
pub fn from_json_str(text: &str) -> Result<Project, LoadError> {
    let value: Value = serde_json::from_str(text).map_err(|e| LoadError::Syntax {
        line: e.line(),
        column: e.column(),
        message: e.to_string(),
    })?;
    let value = migrate(value)?;
    let project: Project =
        serde_json::from_value(value).map_err(|e| LoadError::Shape(e.to_string()))?;
    let issues = validate(&project);
    if issues.is_empty() {
        Ok(project)
    } else {
        Err(LoadError::Invalid(issues))
    }
}

/// Upgrades older format versions to the current one.
fn migrate(value: Value) -> Result<Value, LoadError> {
    let obj = value.as_object().ok_or(LoadError::NotAProject)?;
    if obj.get("format").and_then(Value::as_str) != Some(FORMAT_NAME) {
        return Err(LoadError::NotAProject);
    }
    let version = obj
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| LoadError::Shape("missing format_version".into()))? as u32;
    if version > CURRENT_FORMAT_VERSION {
        return Err(LoadError::TooNew(version));
    }
    // Chain of `migrate_vN_to_vN+1` functions goes here when version 2 exists.
    Ok(value)
}

pub fn load(path: &Path) -> Result<Project, LoadError> {
    let io = |e| LoadError::Io {
        path: path.display().to_string(),
        source: e,
    };
    if fs::metadata(path).map_err(io)?.len() > MAX_PROJECT_BYTES {
        return Err(LoadError::TooLarge);
    }
    let text = fs::read_to_string(path).map_err(io)?;
    from_json_str(&text)
}

/// Deterministic pretty JSON with a trailing newline.
pub fn to_json_string(project: &Project) -> String {
    let mut s = serde_json::to_string_pretty(project).expect("project serialises");
    s.push('\n');
    s
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("refusing to save an invalid project: {}", summarize(.0))]
    Invalid(Vec<Issue>),
    #[error("cannot write {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// Path of the backup kept next to a project file.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Atomically writes `project` to `path` (validated first).
pub fn save(project: &mut Project, path: &Path) -> Result<(), SaveError> {
    project.modified_with = Some(app_version());
    let issues = validate(project);
    if !issues.is_empty() {
        return Err(SaveError::Invalid(issues));
    }
    write_atomic(path, to_json_string(project).as_bytes(), true).map_err(|e| SaveError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

/// Writes bytes atomically: temp file in the same directory → fsync → (backup) → rename.
pub fn write_atomic(path: &Path, bytes: &[u8], keep_backup: bool) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".bubble-save-")
        .tempfile_in(&dir)?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    if keep_backup && path.exists() {
        fs::copy(path, backup_path(path))?;
    }
    // On Windows, antivirus, OneDrive or the search indexer often hold the target for a
    // moment, so replacing it fails with "Access is denied". Retry for about a second
    // before giving up; the temporary file stays intact between attempts.
    let mut attempt = 0;
    loop {
        match tmp.persist(path) {
            Ok(_) => break,
            Err(e)
                if attempt < REPLACE_RETRIES
                    && e.error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                tmp = e.file;
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e.error),
        }
    }
    sync_dir(&dir);
    Ok(())
}

/// Attempts to replace a file another program is holding (100 ms apart).
const REPLACE_RETRIES: u32 = 10;

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}
