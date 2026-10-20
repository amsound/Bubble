//! `dta-import`: converts a reference SpotOn session (`*.dta`) or package (`*.pkg`)
//! into a Bubble project. A package's audio is extracted into `Audio/` next to it.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use dta_import::{DriveMap, ImportOptions, import_file};

/// Import a reference SpotOn session (.dta, format 99) or package (.pkg) into a new
/// Bubble project. A package's audio is extracted into an Audio folder next to it.
///
/// The session file is only read. The project is written to --out, with a migration
/// report next to it (<name>.import-report.json and .md).
#[derive(Parser)]
#[command(name = "dta-import", version)]
struct Args {
    /// Reference session (*.dta) or package (*.pkg).
    session: PathBuf,
    /// New project file (*.bubble).
    #[arg(short, long)]
    out: PathBuf,
    /// Directory to look for audio in: first by each path's tail, then by file name. Repeatable.
    #[arg(short, long = "search", value_name = "DIR")]
    search: Vec<PathBuf>,
    /// Map a Windows drive or UNC share to a local directory, e.g. Y:=/Users/me. Repeatable.
    #[arg(short, long = "map", value_name = "DRIVE=DIR")]
    map: Vec<String>,
    /// Project name (default: the session file name).
    #[arg(long)]
    name: Option<String>,
    /// Overwrite an existing output and accept a session whose integrity check fails.
    #[arg(long)]
    force: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut drives = DriveMap::default();
    for m in &args.map {
        drives.add_spec(m).map_err(anyhow::Error::msg)?;
    }
    let opts = ImportOptions {
        name: args.name,
        search_roots: args.search,
        drives,
        ..Default::default()
    };
    let out = import_file(&args.session, &args.out, &opts, args.force)?;
    let s = &out.report.summary;
    println!("Wrote {}", out.project_path.display());
    println!(
        "  {} of {} slots imported; assets: {} resolved, {} missing; {} setting(s) not carried over",
        s.carts_imported,
        s.slots_total,
        s.assets_resolved,
        s.assets_missing,
        s.settings_unsupported
    );
    for a in &out.report.manual_actions {
        println!("  todo: {a}");
    }
    println!("Report: {}", out.report_md.display());
    println!("        {}", out.report_json.display());
    Ok(())
}
