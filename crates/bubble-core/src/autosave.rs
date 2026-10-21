//! Where unsaved work is kept between autosaves and after a crash.

use std::path::{Path, PathBuf};

/// `<session>.bubble.autosave`, next to the session file.
pub fn autosave_path(project: &Path) -> PathBuf {
    let mut s = project.as_os_str().to_owned();
    s.push(".autosave");
    PathBuf::from(s)
}

/// Folder for autosaves of sessions never saved (`Untitled-<id>.bubble`).
pub fn recovery_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble")
        .map(|d| d.data_dir().join("Recovered"))
}
