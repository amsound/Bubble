//! Translation of the reference app's absolute Windows paths.
//!
//! Order: the path's tail under each `--search` root (longest tail first), then the
//! drive-mapping table (`--map Y:=/Users/me`), then a file-name search of the
//! search roots through [`bubble_project::Resolver`].

use std::path::{Path, PathBuf};

use bubble_project::{AssetRef, Extra, Project, Resolution, Resolver};
use uuid::Uuid;

/// Drive letter or UNC share → local directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriveMap {
    entries: Vec<(String, PathBuf)>,
}

impl DriveMap {
    /// Adds a mapping such as `Y:=/Users/me` or `\\server\share=/Volumes/share`.
    pub fn add_spec(&mut self, spec: &str) -> Result<(), String> {
        let (from, to) = spec
            .split_once('=')
            .ok_or_else(|| format!("expected FROM=TO, got {spec:?}"))?;
        let (prefix, rest) = split_windows(from.trim());
        if prefix.is_none() || !rest.is_empty() {
            return Err(format!(
                "{from:?} is not a drive (Y:) or UNC share (\\\\server\\share)"
            ));
        }
        self.add(prefix.expect("checked"), PathBuf::from(to.trim()));
        Ok(())
    }

    pub fn add(&mut self, prefix: String, dir: PathBuf) {
        self.entries.push((prefix, dir));
    }

    fn lookup(&self, prefix: &str) -> Option<&Path> {
        self.entries
            .iter()
            .find(|(p, _)| p.eq_ignore_ascii_case(prefix))
            .map(|(_, d)| d.as_path())
    }
}

/// Splits a Windows path into its root (`Y:` or `\\server\share`, upper-cased drive)
/// and the remaining non-empty components.
pub fn split_windows(path: &str) -> (Option<String>, Vec<String>) {
    let s = path.replace('/', "\\");
    let (prefix, rest) = if let Some(unc) = s.strip_prefix("\\\\") {
        let mut it = unc.splitn(3, '\\');
        let server = it.next().unwrap_or_default();
        let share = it.next().unwrap_or_default();
        (
            Some(format!("\\\\{server}\\{share}")),
            it.next().unwrap_or_default().to_string(),
        )
    } else if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        (Some(s[..2].to_ascii_uppercase()), s[2..].to_string())
    } else {
        (None, s)
    };
    let parts = rest
        .split('\\')
        .filter(|c| !c.is_empty() && *c != ".")
        .map(str::to_string)
        .collect();
    (prefix, parts)
}

/// The last component of a Windows path.
pub fn windows_file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Where a legacy path was found, or every place that was tried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub found: Option<PathBuf>,
    pub via: Option<&'static str>,
    pub tried: Vec<PathBuf>,
}

pub struct PathTranslator {
    search_roots: Vec<PathBuf>,
    drives: DriveMap,
    resolver: Resolver,
    /// Files extracted from a package: original path (lower-case) → local file.
    packed: Vec<(String, PathBuf)>,
}

impl PathTranslator {
    pub fn new(search_roots: Vec<PathBuf>, drives: DriveMap) -> Self {
        let search_roots: Vec<PathBuf> = search_roots
            .into_iter()
            .map(|r| std::path::absolute(&r).unwrap_or(r))
            .collect();
        let mut probe = Project::new("import");
        probe.asset_search_paths = search_roots
            .iter()
            .map(|r| r.to_string_lossy().to_string())
            .collect();
        let resolver = Resolver::new(None, &probe);
        PathTranslator {
            search_roots,
            drives,
            resolver,
            packed: Vec::new(),
        }
    }

    /// Exact matches for files that came out of a package; tried before anything else.
    pub fn with_packed(mut self, packed: &[(String, PathBuf)]) -> Self {
        self.packed = packed
            .iter()
            .map(|(o, p)| (o.to_lowercase(), p.clone()))
            .collect();
        self
    }

    /// The local path a drive mapping gives for `path`, whether or not it exists.
    pub fn mapped(&self, path: &str) -> Option<PathBuf> {
        let (prefix, parts) = split_windows(path);
        let base = self.drives.lookup(prefix.as_deref()?)?;
        Some(parts.iter().fold(base.to_path_buf(), |p, c| p.join(c)))
    }

    pub fn locate(&mut self, path: &str) -> Located {
        let key = path.to_lowercase();
        if let Some((_, p)) = self.packed.iter().find(|(o, _)| *o == key)
            && p.is_file()
        {
            return Located {
                found: Some(p.clone()),
                via: Some("package"),
                tried: Vec::new(),
            };
        }
        let (_, parts) = split_windows(path);
        let mut tried = Vec::new();
        let hit = |c: PathBuf, tried: &mut Vec<PathBuf>| {
            if c.is_file() {
                Some(c)
            } else {
                if !tried.contains(&c) {
                    tried.push(c);
                }
                None
            }
        };

        for root in &self.search_roots {
            for k in 0..parts.len() {
                let c = parts[k..].iter().fold(root.clone(), |p, c| p.join(c));
                if let Some(p) = hit(c, &mut tried) {
                    return Located {
                        found: Some(p),
                        via: Some("search_root"),
                        tried,
                    };
                }
            }
        }
        if let Some(c) = self.mapped(path)
            && let Some(p) = hit(c, &mut tried)
        {
            return Located {
                found: Some(p),
                via: Some("drive_map"),
                tried,
            };
        }
        if !self.search_roots.is_empty() && !parts.is_empty() {
            // Name search; no fingerprint exists yet, so the first match by name wins.
            let probe = AssetRef {
                id: Uuid::nil(),
                path: parts.last().expect("non-empty").clone(),
                absolute_hint: None,
                fingerprint: None,
                meta: None,
                extra: Extra::new(),
            };
            if let Resolution::Found { path, .. } = self.resolver.resolve(&probe)
                && self.search_roots.iter().any(|r| path.starts_with(r))
            {
                return Located {
                    found: Some(path),
                    via: Some("name_search"),
                    tried,
                };
            }
        }
        Located {
            found: None,
            via: None,
            tried,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_drive_and_unc() {
        assert_eq!(
            split_windows(r"y:\Desktop\a b\x.wav"),
            (
                Some("Y:".into()),
                vec!["Desktop".into(), "a b".into(), "x.wav".into()]
            )
        );
        assert_eq!(
            split_windows(r"\\srv\audio\jingles\x.wav"),
            (
                Some(r"\\srv\audio".into()),
                vec!["jingles".into(), "x.wav".into()]
            )
        );
        assert_eq!(windows_file_name(r"Y:\a\b.wav"), "b.wav");
    }

    #[test]
    fn drive_map_specs() {
        let mut m = DriveMap::default();
        m.add_spec("Y:=/Users/me").unwrap();
        assert!(m.add_spec("Y:\\x=/a").is_err());
        assert!(m.add_spec("nothing").is_err());
        let t = PathTranslator::new(vec![], m);
        assert_eq!(
            t.mapped(r"y:\Desktop\x.wav"),
            Some(PathBuf::from("/Users/me/Desktop/x.wav"))
        );
        assert_eq!(t.mapped(r"C:\x.wav"), None);
    }

    #[test]
    fn tail_under_search_root_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("TestFiles")).unwrap();
        std::fs::write(dir.path().join("TestFiles/a.wav"), b"x").unwrap();
        let mut t = PathTranslator::new(vec![dir.path().to_path_buf()], DriveMap::default());
        let l = t.locate(r"Y:\Desktop\Old\TestFiles\a.wav");
        assert_eq!(l.found, Some(dir.path().join("TestFiles/a.wav")));
        assert_eq!(l.via, Some("search_root"));
        assert_eq!(l.tried.len(), 2);

        std::fs::create_dir_all(dir.path().join("deep/er")).unwrap();
        std::fs::write(dir.path().join("deep/er/b.wav"), b"x").unwrap();
        let l = t.locate(r"Y:\elsewhere\b.wav");
        assert_eq!(l.via, Some("name_search"));
        assert_eq!(l.found, Some(dir.path().join("deep/er/b.wav")));
    }
}
