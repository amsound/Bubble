//! One-time importer for reference SpotOn sessions (`*.dta`, format version 99) and
//! packages (`*.pkg`: a session with its audio) into Bubble projects (`*.bubble`),
//! with a migration report.
//!
//! The source file is only ever read. A package's audio is extracted into an `Audio`
//! folder next to the new project. The project is written atomically through
//! [`bubble_project::save`], which validates it first.

pub mod convert;
pub mod dta;
pub mod paths;
pub mod pkg;
pub mod report;

use std::path::{Path, PathBuf};

use bubble_project::Project;
use sha2::{Digest, Sha256};

pub use convert::{ConvertError, ImportOptions, SourceInfo, convert};
pub use paths::DriveMap;
pub use report::Report;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("not a readable reference session: {0}")]
    Parse(#[from] dta::ParseError),
    #[error(
        "session integrity check failed (trailer token or size mismatch); use --force to import anyway"
    )]
    Trailer,
    #[error(transparent)]
    Convert(#[from] ConvertError),
    #[error("not a readable reference package: {0}")]
    Package(dta::ParseError),
    #[error("the output must be a new *.bubble file, not the source session")]
    OutputIsSource,
    #[error("{0} already exists; use --force to overwrite it")]
    OutputExists(String),
    #[error(transparent)]
    Save(#[from] bubble_project::SaveError),
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
}

/// What [`import_file`] wrote.
#[derive(Debug)]
pub struct ImportOutcome {
    pub project: Project,
    pub report: Report,
    pub project_path: PathBuf,
    pub report_json: PathBuf,
    pub report_md: PathBuf,
}

/// Parses session bytes and converts them, without touching the filesystem except to
/// look for audio files.
pub fn import_bytes(
    bytes: &[u8],
    file_name: &str,
    project_dir: Option<&Path>,
    opts: &ImportOptions,
    force: bool,
) -> Result<(Project, Report), ImportError> {
    let session = dta::parse(bytes)?;
    if !session.trailer_ok && !force {
        return Err(ImportError::Trailer);
    }
    let source = SourceInfo {
        file: file_name.to_string(),
        sha256: sha256_hex(bytes),
    };
    Ok(convert(&session, &source, project_dir, opts)?)
}

/// `<dir>/<name>.import-report.json` and `.md` next to the project file.
pub fn report_paths(project_path: &Path) -> (PathBuf, PathBuf) {
    let stem = project_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "import".into());
    let dir = project_path.parent().unwrap_or(Path::new(""));
    (
        dir.join(format!("{stem}.import-report.json")),
        dir.join(format!("{stem}.import-report.md")),
    )
}

/// Imports `source` into a new project at `out` and writes the reports next to it.
/// With `force`, an existing output is replaced (its previous version is kept as
/// `.bak`) and a failed trailer check is tolerated.
pub fn import_file(
    source: &Path,
    out: &Path,
    opts: &ImportOptions,
    force: bool,
) -> Result<ImportOutcome, ImportError> {
    let same = match (std::fs::canonicalize(source), std::fs::canonicalize(out)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same
        || out
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dta") || e.eq_ignore_ascii_case("pkg"))
    {
        return Err(ImportError::OutputIsSource);
    }
    if out.exists() && !force {
        return Err(ImportError::OutputExists(out.display().to_string()));
    }
    let bytes = std::fs::read(source).map_err(|e| ImportError::Read {
        path: source.display().to_string(),
        source: e,
    })?;
    let file_name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let project_dir = match out.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let (mut project, report) = if pkg::is_package(&bytes) {
        import_package_bytes(&bytes, &file_name, &project_dir, opts, force)?
    } else {
        import_bytes(&bytes, &file_name, Some(&project_dir), opts, force)?
    };
    bubble_project::save(&mut project, out)?;

    let (report_json, report_md) = report_paths(out);
    for (path, text) in [
        (&report_json, report.to_json()),
        (&report_md, report.to_markdown()),
    ] {
        bubble_project::io::write_atomic(path, text.as_bytes(), false).map_err(|e| {
            ImportError::Write {
                path: path.display().to_string(),
                source: e,
            }
        })?;
    }
    Ok(ImportOutcome {
        project,
        report,
        project_path: out.to_path_buf(),
        report_json,
        report_md,
    })
}

/// Extracts a package's audio into `<project_dir>/Audio` and converts its session.
/// Existing identical files are reused; a different file with the same name gets a
/// numbered name, so nothing is overwritten.
pub fn import_package_bytes(
    bytes: &[u8],
    file_name: &str,
    project_dir: &Path,
    opts: &ImportOptions,
    force: bool,
) -> Result<(Project, Report), ImportError> {
    let package = pkg::parse(bytes).map_err(ImportError::Package)?;
    if !package.session.trailer_ok && !force {
        return Err(ImportError::Trailer);
    }
    let audio_dir = project_dir.join("Audio");
    std::fs::create_dir_all(&audio_dir).map_err(|e| ImportError::Write {
        path: audio_dir.display().to_string(),
        source: e,
    })?;
    let mut packed = Vec::new();
    for f in &package.files {
        let data = package.bytes(bytes, f);
        let target = free_name(&audio_dir, &pkg::file_name_of(&f.original_path), data);
        if !target.exists() {
            bubble_project::io::write_atomic(&target, data, false).map_err(|e| {
                ImportError::Write {
                    path: target.display().to_string(),
                    source: e,
                }
            })?;
        }
        packed.push((f.original_path.clone(), target));
    }
    let mut opts = opts.clone();
    opts.packed = packed;
    let source = SourceInfo {
        file: file_name.to_string(),
        sha256: sha256_hex(bytes),
    };
    let (project, mut report) = convert(&package.session, &source, Some(project_dir), &opts)?;
    if !package.options_found {
        report.manual_actions.push(
            "The package's session options (default fade-out, hot-keys on/off) could not be \
             read; project defaults were used."
                .into(),
        );
        report.summary.manual_actions = report.manual_actions.len();
    }
    Ok((project, report))
}

/// `dir/name`, or `dir/name (2).ext`… when a different file already has that name.
/// Returns an existing path when its content is identical (so it is reused).
fn free_name(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 1.. {
        let candidate = if n == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match std::fs::read(&candidate) {
            Err(_) => return candidate,
            Ok(existing) if existing == data => return candidate,
            Ok(_) => {}
        }
    }
    unreachable!()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
