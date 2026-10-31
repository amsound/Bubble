//! Importing from a file: progress and cancelling.

use std::sync::atomic::Ordering::Relaxed;

use dta_import::{ImportControl, ImportError, ImportOptions, import_source};

#[test]
fn a_session_that_cannot_be_read_says_so_and_cancelling_stops_a_package() {
    let dir = tempfile::tempdir().unwrap();
    let opts = ImportOptions::default();
    let missing = import_source(
        &dir.path().join("gone.pkg"),
        "gone.pkg",
        dir.path(),
        &opts,
        false,
    );
    assert!(matches!(missing, Err(ImportError::Read { .. })));

    let pkg = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pkg");
    if !pkg.exists() {
        return;
    }
    let control = ImportControl::default();
    control.cancel.store(true, Relaxed);
    let opts = ImportOptions {
        control: Some(control),
        ..Default::default()
    };
    let cancelled = import_source(&pkg, "sample.pkg", dir.path(), &opts, true);
    assert!(matches!(cancelled, Err(ImportError::Cancelled)));
    let audio: Vec<_> = std::fs::read_dir(dir.path().join("Audio"))
        .unwrap()
        .collect();
    assert!(audio.is_empty());
}
