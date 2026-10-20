//! Portable asset references and resolution.
//!
//! Paths in a project are stored POSIX-style (`/`) and relative to the project
//! directory whenever possible. Resolution order:
//! 1. stored path (relative to the project directory, or absolute);
//! 2. `absolute_hint` when it was recorded on this platform;
//! 3. search roots (`asset_search_paths`), by file name, accepted only when the
//!    fingerprint matches (or when no fingerprint is stored).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

use crate::fingerprint::{self, Match};
use crate::model::{AbsoluteHint, AssetRef, Extra, Platform, Project};

/// Converts a filesystem path to the portable string form.
pub fn to_portable(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

/// Converts a portable string back to a platform path.
pub fn from_portable(s: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(s.replace('/', "\\"))
    } else {
        PathBuf::from(s)
    }
}

fn is_absolute_portable(s: &str) -> bool {
    s.starts_with('/')
        || s.starts_with("\\\\")
        || (s.len() > 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic())
}

/// Lexically normalises `.` and `..` components.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Portable relative path from `base_dir` to `target` (both absolute), or `None`
/// when they do not share a root (e.g. different Windows drives).
pub fn relative_path(target: &Path, base_dir: &Path) -> Option<String> {
    let t = normalize(target);
    let b = normalize(base_dir);
    let tc: Vec<_> = t.components().collect();
    let bc: Vec<_> = b.components().collect();
    if tc.first() != bc.first() {
        return None;
    }
    let common = tc.iter().zip(&bc).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..bc.len() {
        parts.push("..".into());
    }
    for c in &tc[common..] {
        parts.push(c.as_os_str().to_string_lossy().to_string());
    }
    if parts.is_empty() {
        return Some(".".into());
    }
    Some(parts.join("/"))
}

/// Builds an [`AssetRef`] for a file, relative to `project_dir` when given.
pub fn make_asset_ref(project_dir: Option<&Path>, file: &Path) -> std::io::Result<AssetRef> {
    let abs = std::path::absolute(file)?;
    let path = match project_dir {
        Some(dir) => {
            relative_path(&abs, &std::path::absolute(dir)?).unwrap_or_else(|| to_portable(&abs))
        }
        None => to_portable(&abs),
    };
    Ok(AssetRef {
        id: Uuid::new_v4(),
        path,
        absolute_hint: Some(AbsoluteHint {
            platform: Platform::current(),
            path: abs.to_string_lossy().to_string(),
        }),
        fingerprint: Some(fingerprint::fingerprint_file(&abs)?),
        meta: None,
        extra: Extra::new(),
    })
}

/// Rewrites stored paths relative to a new project directory (used by Save As).
pub fn rebase_paths(project: &mut Project, old_dir: Option<&Path>, new_dir: &Path) {
    for a in &mut project.assets {
        let current = match old_dir {
            Some(d) if !is_absolute_portable(&a.path) => d.join(from_portable(&a.path)),
            _ => from_portable(&a.path),
        };
        if let Ok(abs) = std::path::absolute(&current)
            && let Some(rel) = relative_path(&abs, new_dir)
        {
            a.path = rel;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    StoredPath,
    AbsoluteHint,
    Search,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Found {
        path: PathBuf,
        matched: Match,
        via: Via,
    },
    Missing {
        tried: Vec<PathBuf>,
    },
}

impl Resolution {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Resolution::Found { path, .. } => Some(path),
            Resolution::Missing { .. } => None,
        }
    }
}

/// Resolves the assets of one project.
pub struct Resolver {
    project_dir: Option<PathBuf>,
    search_roots: Vec<PathBuf>,
    /// File name (lower-case) → candidate paths, built lazily from the search roots.
    index: Option<HashMap<String, Vec<PathBuf>>>,
}

const SEARCH_MAX_DEPTH: usize = 6;
const SEARCH_MAX_ENTRIES: usize = 100_000;

impl Resolver {
    pub fn new(project_dir: Option<&Path>, project: &Project) -> Self {
        let search_roots = project
            .asset_search_paths
            .iter()
            .filter_map(|s| {
                let p = from_portable(s);
                if p.is_absolute() {
                    Some(p)
                } else {
                    project_dir.map(|d| normalize(&d.join(p)))
                }
            })
            .collect();
        Resolver {
            project_dir: project_dir.map(Path::to_path_buf),
            search_roots,
            index: None,
        }
    }

    /// The absolute location the stored path points to.
    pub fn stored_location(&self, asset: &AssetRef) -> PathBuf {
        let p = from_portable(&asset.path);
        if is_absolute_portable(&asset.path) {
            p
        } else if let Some(d) = &self.project_dir {
            normalize(&d.join(p))
        } else {
            p
        }
    }

    pub fn resolve(&mut self, asset: &AssetRef) -> Resolution {
        let mut tried = Vec::new();
        let stored = self.stored_location(asset);
        if stored.is_file() {
            return found(stored, asset, Via::StoredPath);
        }
        tried.push(stored);
        if let Some(h) = &asset.absolute_hint
            && h.platform == Platform::current()
        {
            let p = PathBuf::from(&h.path);
            if p.is_file() {
                return found(p, asset, Via::AbsoluteHint);
            }
            tried.push(p);
        }
        let name = file_name_lower(&asset.path);
        let candidates = self.index().get(&name).cloned().unwrap_or_default();
        for c in candidates {
            match fingerprint::check(&c, asset.fingerprint.as_ref()) {
                Ok(Match::Exact) => {
                    return Resolution::Found {
                        path: c,
                        matched: Match::Exact,
                        via: Via::Search,
                    };
                }
                Ok(Match::Unverified) => {
                    return Resolution::Found {
                        path: c,
                        matched: Match::Unverified,
                        via: Via::Search,
                    };
                }
                _ => tried.push(c),
            }
        }
        Resolution::Missing { tried }
    }

    fn index(&mut self) -> &HashMap<String, Vec<PathBuf>> {
        if self.index.is_none() {
            let mut map: HashMap<String, Vec<PathBuf>> = HashMap::new();
            let mut budget = SEARCH_MAX_ENTRIES;
            for root in &self.search_roots {
                walk(root, 0, &mut budget, &mut map);
            }
            self.index = Some(map);
        }
        self.index.as_ref().expect("built above")
    }
}

fn found(path: PathBuf, asset: &AssetRef, via: Via) -> Resolution {
    let matched =
        fingerprint::check(&path, asset.fingerprint.as_ref()).unwrap_or(Match::Unverified);
    Resolution::Found { path, matched, via }
}

fn file_name_lower(portable: &str) -> String {
    portable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(portable)
        .to_lowercase()
}

fn walk(dir: &Path, depth: usize, budget: &mut usize, map: &mut HashMap<String, Vec<PathBuf>>) {
    if depth > SEARCH_MAX_DEPTH || *budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            let hidden = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if !hidden {
                walk(&p, depth + 1, budget, map);
            }
        } else if ft.is_file()
            && let Some(n) = p.file_name()
        {
            map.entry(n.to_string_lossy().to_lowercase())
                .or_default()
                .push(p);
        }
    }
}

/// Outcome of a relink operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelinkReport {
    /// Assets whose path was updated (including the one relinked explicitly).
    pub relinked: Vec<Uuid>,
}

/// Points `asset_id` at `new_file`, then tries to relink every other asset that is
/// currently missing using the same directory move.
pub fn relink(
    project: &mut Project,
    project_dir: Option<&Path>,
    asset_id: Uuid,
    new_file: &Path,
) -> std::io::Result<RelinkReport> {
    let mut report = RelinkReport::default();
    let new_abs = std::path::absolute(new_file)?;
    let resolver = Resolver::new(project_dir, project);
    let Some(asset) = project.asset(asset_id).cloned() else {
        return Ok(report);
    };
    let old_abs = resolver.stored_location(&asset);

    // Work out the directory mapping old_prefix → new_prefix from the common suffix.
    let oc: Vec<_> = old_abs.components().collect();
    let nc: Vec<_> = new_abs.components().collect();
    let suffix = oc
        .iter()
        .rev()
        .zip(nc.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_prefix: PathBuf = oc[..oc.len() - suffix.max(1).min(oc.len())]
        .iter()
        .collect();
    let new_prefix: PathBuf = nc[..nc.len() - suffix.max(1).min(nc.len())]
        .iter()
        .collect();

    let set_path = |a: &mut AssetRef, abs: &Path| {
        a.path = project_dir
            .and_then(|d| std::path::absolute(d).ok())
            .and_then(|d| relative_path(abs, &d))
            .unwrap_or_else(|| to_portable(abs));
        a.absolute_hint = Some(AbsoluteHint {
            platform: Platform::current(),
            path: abs.to_string_lossy().to_string(),
        });
    };

    {
        let fp = fingerprint::fingerprint_file(&new_abs)?;
        let a = project.asset_mut(asset_id).expect("checked above");
        set_path(a, &new_abs);
        a.fingerprint = Some(fp);
        report.relinked.push(asset_id);
    }

    let new_dir = new_abs.parent().map(Path::to_path_buf);
    let others: Vec<AssetRef> = project
        .assets
        .iter()
        .filter(|a| a.id != asset_id)
        .cloned()
        .collect();
    let mut resolver = Resolver::new(project_dir, project);
    for other in others {
        if matches!(resolver.resolve(&other), Resolution::Found { .. }) {
            continue;
        }
        let loc = resolver.stored_location(&other);
        let mut candidates = Vec::new();
        if let Ok(rest) = loc.strip_prefix(&old_prefix) {
            candidates.push(new_prefix.join(rest));
        }
        if let (Some(d), Some(n)) = (&new_dir, loc.file_name()) {
            candidates.push(d.join(n));
        }
        for c in candidates {
            if !c.is_file() {
                continue;
            }
            let ok = matches!(
                fingerprint::check(&c, other.fingerprint.as_ref()),
                Ok(Match::Exact | Match::Unverified)
            );
            if ok {
                let a = project.asset_mut(other.id).expect("exists");
                set_path(a, &c);
                report.relinked.push(other.id);
                break;
            }
        }
    }
    Ok(report)
}
