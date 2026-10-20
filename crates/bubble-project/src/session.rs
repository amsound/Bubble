//! Session folders: everything a show needs in one place.
//!
//! ```text
//! Friday Night Show 2026/
//! ├── Friday Night Show 2026.bubble
//! └── Audio/
//!     ├── Sting.wav
//!     └── …
//! ```
//!
//! Audio brought into a session (dropped, assigned, relinked, imported) is copied into
//! `Audio/`, so a session can be moved, backed up or handed over as one folder. Copies
//! never overwrite: an identical file already there is reused, a different file with
//! the same name gets a numbered name.

use std::io;
use std::path::{Path, PathBuf};

use crate::io::EXTENSION;

/// Sub-folder holding a session's audio.
pub const AUDIO_DIR: &str = "Audio";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("the session needs a name")]
    NoName,
    #[error("{0} already exists; choose another name or location")]
    Exists(PathBuf),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> SessionError + '_ {
    move |source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A name usable as a folder and file name on macOS, Windows and Linux.
pub fn clean_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    s.trim().trim_matches('.').trim().to_string()
}

/// `<parent>/<name>/<name>.bubble` for a session called `name`.
pub fn session_file(parent: &Path, name: &str) -> PathBuf {
    let n = clean_name(name);
    parent.join(&n).join(format!("{n}.{EXTENSION}"))
}

/// Creates `<parent>/<name>/` with an empty `Audio/` folder and returns the path the
/// session file should be saved to. Refuses an existing, non-empty folder.
pub fn create_session_folder(parent: &Path, name: &str) -> Result<PathBuf, SessionError> {
    if clean_name(name).is_empty() {
        return Err(SessionError::NoName);
    }
    let file = session_file(parent, name);
    let dir = file
        .parent()
        .expect("session file has a folder")
        .to_path_buf();
    if dir.exists()
        && std::fs::read_dir(&dir)
            .map_err(io_err(&dir))?
            .next()
            .is_some()
    {
        return Err(SessionError::Exists(dir));
    }
    std::fs::create_dir_all(dir.join(AUDIO_DIR)).map_err(io_err(&dir))?;
    Ok(file)
}

/// Whether `path` is inside `dir` (both made absolute; no symlink resolution).
pub fn is_inside(path: &Path, dir: &Path) -> bool {
    match (std::path::absolute(path), std::path::absolute(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

/// Copies `src` into `<session_dir>/Audio/` and returns the copy's path. A file
/// already inside the session is returned unchanged; an identical file of the same
/// name is reused; a different one is copied as `name (2).ext`, `name (3).ext`, ….
pub fn collect_file(session_dir: &Path, src: &Path) -> Result<PathBuf, SessionError> {
    if is_inside(src, session_dir) {
        return Ok(src.to_path_buf());
    }
    let audio = session_dir.join(AUDIO_DIR);
    std::fs::create_dir_all(&audio).map_err(io_err(&audio))?;
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio".into());
    let p = Path::new(&name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let src_len = std::fs::metadata(src).map_err(io_err(src))?.len();
    for n in 1u32.. {
        let target = if n == 1 {
            audio.join(&name)
        } else {
            audio.join(format!("{stem} ({n}){ext}"))
        };
        match std::fs::metadata(&target) {
            Err(_) => {
                // Copy to a temporary name first so a cancelled or failed copy never
                // leaves a truncated file under the real name.
                let tmp = audio.join(format!(
                    ".{}.part",
                    target.file_name().unwrap().to_string_lossy()
                ));
                std::fs::copy(src, &tmp).map_err(io_err(src))?;
                std::fs::rename(&tmp, &target).map_err(io_err(&target))?;
                return Ok(target);
            }
            Ok(m) if m.len() == src_len && same_content(src, &target)? => return Ok(target),
            Ok(_) => {}
        }
    }
    unreachable!()
}

fn same_content(a: &Path, b: &Path) -> Result<bool, SessionError> {
    use std::io::Read;
    let mut fa = std::fs::File::open(a).map_err(io_err(a))?;
    let mut fb = std::fs::File::open(b).map_err(io_err(b))?;
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let na = fa.read(&mut ba).map_err(io_err(a))?;
        let mut filled = 0;
        while filled < na {
            let nb = fb.read(&mut bb[filled..na]).map_err(io_err(b))?;
            if nb == 0 {
                return Ok(false);
            }
            filled += nb;
        }
        if na == 0 {
            return Ok(fb.read(&mut bb[..1]).map_err(io_err(b))? == 0);
        }
        if ba[..na] != bb[..na] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(
            clean_name("  Friday Night: 26/27? "),
            "Friday Night- 26-27-"
        );
        assert_eq!(clean_name("..."), "");
    }

    #[test]
    fn create_collect_and_dedupe() {
        let t = tempfile::tempdir().unwrap();
        let file = create_session_folder(t.path(), "Show 1").unwrap();
        assert_eq!(file, t.path().join("Show 1/Show 1.bubble"));
        assert!(t.path().join("Show 1/Audio").is_dir());
        // An existing non-empty folder is refused.
        std::fs::write(t.path().join("Show 1/x"), b"x").unwrap();
        assert!(matches!(
            create_session_folder(t.path(), "Show 1"),
            Err(SessionError::Exists(_))
        ));
        assert!(matches!(
            create_session_folder(t.path(), " .. "),
            Err(SessionError::NoName)
        ));

        let dir = file.parent().unwrap();
        let src = t.path().join("elsewhere");
        std::fs::create_dir_all(src.join("b")).unwrap();
        std::fs::write(src.join("a.wav"), b"one").unwrap();
        std::fs::write(src.join("b/a.wav"), b"two").unwrap();
        let c1 = collect_file(dir, &src.join("a.wav")).unwrap();
        assert_eq!(c1, dir.join("Audio/a.wav"));
        // Same file again: reused.
        assert_eq!(collect_file(dir, &src.join("a.wav")).unwrap(), c1);
        // Different content, same name: numbered, original untouched.
        let c2 = collect_file(dir, &src.join("b/a.wav")).unwrap();
        assert_eq!(c2, dir.join("Audio/a (2).wav"));
        assert_eq!(std::fs::read(&c1).unwrap(), b"one");
        assert_eq!(std::fs::read(&c2).unwrap(), b"two");
        // Already inside the session: unchanged.
        assert_eq!(collect_file(dir, &c2).unwrap(), c2);
        // No temporary files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.join("Audio"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".part")), "{names:?}");
    }
}
