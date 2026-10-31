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

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

use bubble_project::Project;
use sha2::{Digest, Sha256};

pub use convert::{ConvertError, ImportControl, ImportOptions, SourceInfo, convert};
pub use paths::DriveMap;
pub use report::Report;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("cancelled")]
    Cancelled,
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
    let file_name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let project_dir = match out.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let (mut project, report) = import_source(source, &file_name, &project_dir, opts, force)?;
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

/// Converts a session or package from its file. A package may be many gigabytes, so
/// it is not read into memory: its audio is copied across in pieces.
pub fn import_source(
    source: &Path,
    file_name: &str,
    project_dir: &Path,
    opts: &ImportOptions,
    force: bool,
) -> Result<(Project, Report), ImportError> {
    let err = |e| ImportError::Read {
        path: source.display().to_string(),
        source: e,
    };
    let mut file = std::fs::File::open(source).map_err(err)?;
    let mut start = [0u8; 32];
    let package = file.read_exact(&mut start).is_ok() && pkg::is_package(&start);
    if package {
        import_package_from(&mut file, file_name, project_dir, opts, force)
    } else {
        let bytes = read_source(source, opts.control.as_ref())?;
        import_bytes(&bytes, file_name, Some(project_dir), opts, force)
    }
}

/// Reads a session file (never large) to import.
fn read_source(source: &Path, control: Option<&ImportControl>) -> Result<Vec<u8>, ImportError> {
    let bytes = std::fs::read(source).map_err(|e| ImportError::Read {
        path: source.display().to_string(),
        source: e,
    })?;
    if let Some(c) = control {
        c.total.store(bytes.len() as u64, Relaxed);
        c.done.store(bytes.len() as u64, Relaxed);
    }
    Ok(bytes)
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
    let mut input = std::io::Cursor::new(bytes);
    import_package_from(&mut input, file_name, project_dir, opts, force)
}

/// How much of a package is held in memory at a time.
const PIECE: usize = 1 << 20;

/// [`import_package_bytes`] from anything that can be read and sought, a file above all.
/// Its fingerprint for the report is the first half of the work, and copying out its
/// audio the second, a package's size being mostly audio.
fn import_package_from<R: Read + Seek>(
    input: &mut R,
    file_name: &str,
    project_dir: &Path,
    opts: &ImportOptions,
    force: bool,
) -> Result<(Project, Report), ImportError> {
    let read_err = |e| ImportError::Read {
        path: file_name.to_string(),
        source: e,
    };
    let package = pkg::parse_from(input).map_err(|e| match e {
        pkg::ReadError::Parse(e) => ImportError::Package(e),
        pkg::ReadError::Io(e) => read_err(e),
    })?;
    if !package.session.trailer_ok && !force {
        return Err(ImportError::Trailer);
    }
    let audio_dir = project_dir.join("Audio");
    std::fs::create_dir_all(&audio_dir).map_err(|e| ImportError::Write {
        path: audio_dir.display().to_string(),
        source: e,
    })?;
    let control = opts.control.as_ref();
    let size = input.seek(SeekFrom::End(0)).map_err(read_err)?;
    if let Some(c) = control {
        c.total.store(size * 2, Relaxed);
    }

    let mut piece = vec![0u8; PIECE];
    let mut hash = Sha256::new();
    input.seek(SeekFrom::Start(0)).map_err(read_err)?;
    loop {
        stop_if_cancelled(control)?;
        let n = input.read(&mut piece).map_err(read_err)?;
        if n == 0 {
            break;
        }
        hash.update(&piece[..n]);
        advance(control, n);
    }
    let sha256 = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();

    let mut packed = Vec::new();
    for f in &package.files {
        let target = extract(input, f, &audio_dir, control, &mut piece, &read_err)?;
        packed.push((f.original_path.clone(), target));
    }
    let mut opts = opts.clone();
    opts.packed = packed;
    let source = SourceInfo {
        file: file_name.to_string(),
        sha256,
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

fn stop_if_cancelled(control: Option<&ImportControl>) -> Result<(), ImportError> {
    if control.is_some_and(|c| c.cancel.load(Relaxed)) {
        return Err(ImportError::Cancelled);
    }
    Ok(())
}

fn advance(control: Option<&ImportControl>, n: usize) {
    if let Some(c) = control {
        c.done.fetch_add(n as u64, Relaxed);
    }
}

/// Writes a packed file to `dir/name`, or `dir/name (2).ext`… when a different file
/// already has that name. An identical file already there is reused. Written beside its
/// final name first, so a cancelled or failed copy leaves nothing behind.
fn extract<R: Read + Seek>(
    input: &mut R,
    file: &pkg::PackedFile,
    dir: &Path,
    control: Option<&ImportControl>,
    piece: &mut [u8],
    read_err: &dyn Fn(std::io::Error) -> ImportError,
) -> Result<PathBuf, ImportError> {
    let name = pkg::file_name_of(&file.original_path);
    let p = Path::new(&name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let len = file.data.len() as u64;
    let start = file.data.start as u64;
    for n in 1.. {
        let candidate = if n == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        let Ok(meta) = std::fs::metadata(&candidate) else {
            write_piecewise(input, start, len, &candidate, control, piece, read_err)?;
            return Ok(candidate);
        };
        if meta.len() == len && same_content(input, start, len, &candidate, piece, read_err)? {
            advance(control, len as usize);
            return Ok(candidate);
        }
    }
    unreachable!()
}

fn write_piecewise<R: Read + Seek>(
    input: &mut R,
    start: u64,
    len: u64,
    target: &Path,
    control: Option<&ImportControl>,
    piece: &mut [u8],
    read_err: &dyn Fn(std::io::Error) -> ImportError,
) -> Result<(), ImportError> {
    let mut part = target.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let write_err = |e| ImportError::Write {
        path: target.display().to_string(),
        source: e,
    };
    let mut copy = || -> Result<(), ImportError> {
        let mut out = std::fs::File::create(&part).map_err(write_err)?;
        input.seek(SeekFrom::Start(start)).map_err(read_err)?;
        let mut left = len;
        while left > 0 {
            stop_if_cancelled(control)?;
            let want = left.min(piece.len() as u64) as usize;
            input.read_exact(&mut piece[..want]).map_err(read_err)?;
            out.write_all(&piece[..want]).map_err(write_err)?;
            left -= want as u64;
            advance(control, want);
        }
        out.sync_all().map_err(write_err)?;
        drop(out);
        std::fs::rename(&part, target).map_err(write_err)
    };
    let result = copy();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn same_content<R: Read + Seek>(
    input: &mut R,
    start: u64,
    len: u64,
    other: &Path,
    piece: &mut [u8],
    read_err: &dyn Fn(std::io::Error) -> ImportError,
) -> Result<bool, ImportError> {
    let Ok(mut file) = std::fs::File::open(other) else {
        return Ok(false);
    };
    input.seek(SeekFrom::Start(start)).map_err(read_err)?;
    let mut theirs = vec![0u8; piece.len()];
    let mut left = len;
    while left > 0 {
        let want = left.min(piece.len() as u64) as usize;
        input.read_exact(&mut piece[..want]).map_err(read_err)?;
        if file.read_exact(&mut theirs[..want]).is_err() || piece[..want] != theirs[..want] {
            return Ok(false);
        }
        left -= want as u64;
    }
    Ok(true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
