//! Package (`*.pkg`) import, on a fixture cut down from a real package saved by the
//! reference app: its header, the first two button records and its options tail are
//! unchanged; the two 3.5 MB test tones were replaced by small test files.

use std::path::{Path, PathBuf};

use bubble_project::validate;
use dta_import::{ImportOptions, import_file, pkg};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package.pkg")
}

/// Whether the SpotOn package fixture or `TestFiles/` is absent (neither is
/// distributed with the source); tests that need them skip.
fn missing_fixtures() -> bool {
    let missing = !fixture().exists()
        || !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../TestFiles")
            .is_dir();
    if missing {
        eprintln!("skipped: package fixture or TestFiles/ not present");
    }
    missing
}

fn test_file(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../TestFiles")
            .join(name),
    )
    .unwrap()
}

#[test]
fn parses_header_files_records_and_options() {
    if missing_fixtures() {
        return;
    }
    let d = std::fs::read(fixture()).unwrap();
    assert!(pkg::is_package(&d));
    let p = pkg::parse(&d).unwrap();
    assert!(p.session.trailer_ok);
    assert!(p.options_found, "FileSavePost located after the images");
    assert_eq!(p.session.page_names.len(), 8);
    assert_eq!((p.session.grid_columns, p.session.grid_rows), (5, 3));
    assert_eq!(p.files.len(), 2);
    assert!(
        p.files[0]
            .original_path
            .ends_with(r"TestFiles\100Hz_16_44_AB_0.wav")
    );
    assert_eq!(p.bytes(&d, &p.files[0]), test_file("ding_16_44_AB.wav"));
    assert_eq!(p.bytes(&d, &p.files[1]), test_file("ding_16_48_AB.wav"));
    let idx: Vec<u16> = p.session.carts.iter().map(|c| c.index).collect();
    assert_eq!(idx, [0, 1]);
}

#[test]
fn rejects_a_session_file_and_a_truncated_package() {
    if missing_fixtures() {
        return;
    }
    let d = std::fs::read(fixture()).unwrap();
    // Cut inside the audio: unreadable.
    assert!(pkg::parse(&d[..200_000]).is_err());
    // Cut after the records: readable, but the integrity check fails.
    let cut = pkg::parse(&d[..d.len() - 1000]).unwrap();
    assert!(!cut.session.trailer_ok);
    let dta =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session.dta"))
            .unwrap();
    assert!(!pkg::is_package(&dta));
}

#[test]
fn import_extracts_audio_next_to_the_project() {
    if missing_fixtures() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("Show.bubble");
    let o = import_file(&fixture(), &out, &ImportOptions::default(), false).unwrap();
    assert!(
        validate(&o.project).is_empty(),
        "{:?}",
        validate(&o.project)
    );
    assert_eq!(o.report.summary.carts_imported, 2);
    assert_eq!(o.report.summary.assets_missing, 0);
    let audio = dir.path().join("Audio");
    assert_eq!(
        std::fs::read(audio.join("100Hz_16_44_AB_0.wav")).unwrap(),
        test_file("ding_16_44_AB.wav")
    );
    // Asset paths are relative to the project.
    let paths: Vec<&str> = o.project.assets.iter().map(|a| a.path.as_str()).collect();
    assert!(paths.contains(&"Audio/100Hz_16_44_AB_0.wav"), "{paths:?}");

    // Importing again reuses identical files; a different file with the same name is
    // never overwritten.
    std::fs::write(audio.join("100Hz_16_48_AB_0.wav"), b"something else").unwrap();
    let out2 = dir.path().join("Again.bubble");
    let o2 = import_file(&fixture(), &out2, &ImportOptions::default(), false).unwrap();
    assert_eq!(o2.report.summary.assets_missing, 0);
    assert_eq!(
        std::fs::read(audio.join("100Hz_16_48_AB_0.wav")).unwrap(),
        b"something else"
    );
    assert_eq!(
        std::fs::read(audio.join("100Hz_16_48_AB_0 (2).wav")).unwrap(),
        test_file("ding_16_48_AB.wav")
    );
    assert!(!audio.join("100Hz_16_44_AB_0 (2).wav").exists());
}
