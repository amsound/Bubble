//! Sessions in cloud-synced folders.
//!
//! iCloud Drive (including Desktop & Documents sync), OneDrive, Dropbox and Google Drive
//! can replace files with online-only placeholders to save space. Bubble streams long
//! files from disk, so a placeholder would make playback wait for a download. Sessions
//! in such folders get a warning, and online-only audio files a clear one.

use std::path::{Path, PathBuf};

/// The sync service whose folder holds `path`, if any.
pub(super) fn sync_service(path: &Path) -> Option<&'static str> {
    let s = path.to_string_lossy().replace('\\', "/");
    let lower = s.to_lowercase();
    if lower.contains("/library/mobile documents/") {
        return Some("iCloud Drive");
    }
    if let Some(rest) = lower.split("/library/cloudstorage/").nth(1) {
        return Some(if rest.starts_with("onedrive") {
            "OneDrive"
        } else if rest.starts_with("dropbox") {
            "Dropbox"
        } else if rest.starts_with("googledrive") {
            "Google Drive"
        } else {
            "a cloud storage service"
        });
    }
    if lower.contains("/dropbox/") {
        return Some("Dropbox");
    }
    if lower.contains("/google drive/") {
        return Some("Google Drive");
    }
    #[cfg(windows)]
    for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(root) = std::env::var_os(var) {
            let root = PathBuf::from(root)
                .to_string_lossy()
                .replace('\\', "/")
                .to_lowercase();
            if !root.is_empty() && lower.starts_with(&root) {
                return Some("OneDrive");
            }
        }
    }
    // macOS "Desktop & Documents Folders" in iCloud: the folders keep their usual
    // paths, and appear inside iCloud Drive.
    #[cfg(target_os = "macos")]
    if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
        let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        for folder in ["Desktop", "Documents"] {
            if path.starts_with(home.join(folder)) && icloud.join(folder).exists() {
                return Some("iCloud Drive (Desktop & Documents)");
            }
        }
    }
    None
}

/// Whether `path` is an online-only placeholder (its data is not on this computer).
pub(super) fn online_only(path: &Path) -> bool {
    let Ok(m) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const SF_DATALESS: u32 = 0x4000_0000;
        m.st_flags() & SF_DATALESS != 0
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const OFFLINE: u32 = 0x1000;
        const RECALL_ON_OPEN: u32 = 0x4_0000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
        m.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = m;
        false
    }
}

/// How the warning should be shown.
pub(super) struct CloudCheck {
    /// One line for the status bar.
    pub status: String,
    /// A fuller explanation when files are already online-only.
    pub alert: Option<String>,
}

/// Checks a session file and its audio.
pub(super) fn check(session: &Path, audio: &[PathBuf]) -> Option<CloudCheck> {
    let offline: Vec<&PathBuf> = audio.iter().filter(|p| online_only(p)).collect();
    let service = sync_service(session).or_else(|| audio.iter().find_map(|p| sync_service(p)));
    if offline.is_empty() && service.is_none() {
        return None;
    }
    let service = service.unwrap_or("a cloud storage service");
    let keep = if cfg!(target_os = "macos") {
        "In Finder, right-click the session folder and choose Keep Downloaded"
    } else {
        "In File Explorer, right-click the session folder and choose Always keep on this device"
    };
    if offline.is_empty() {
        return Some(CloudCheck {
            status: format!("Session is in a folder synced by {service}: keep it downloaded"),
            alert: None,
        });
    }
    let names: Vec<String> = offline
        .iter()
        .take(5)
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .collect();
    Some(CloudCheck {
        status: format!(
            "{} audio file(s) are online-only ({service})",
            offline.len()
        ),
        alert: Some(format!(
            "{} audio file(s) in this session are stored online only by {service}, so \
             playing them would have to wait for a download:\n\n{}{}\n\n\
             {keep}, or move the session to a folder that is not synced (for example \
             the Bubble folder in your home folder).",
            offline.len(),
            names.join("\n"),
            if offline.len() > names.len() {
                "\n…"
            } else {
                ""
            }
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::sync_service;
    use std::path::Path;

    #[test]
    fn recognises_sync_folders() {
        assert_eq!(
            sync_service(Path::new(
                "/Users/me/Library/Mobile Documents/com~apple~CloudDocs/Shows/A.bubble"
            )),
            Some("iCloud Drive")
        );
        assert_eq!(
            sync_service(Path::new(
                "/Users/me/Library/CloudStorage/OneDrive-Personal/A.bubble"
            )),
            Some("OneDrive")
        );
        assert_eq!(
            sync_service(Path::new("/Users/me/Dropbox/Show/A.bubble")),
            Some("Dropbox")
        );
        assert_eq!(sync_service(Path::new("/Volumes/Shows/A/A.bubble")), None);
    }
}
