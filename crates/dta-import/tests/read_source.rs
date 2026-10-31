//! Reading what is to be imported: progress and cancelling.

use std::sync::atomic::Ordering::Relaxed;

use dta_import::{ImportControl, ImportError, read_source};

#[test]
fn reading_reports_progress_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("s.pkg");
    std::fs::write(&file, vec![7u8; 100_000]).unwrap();

    let control = ImportControl::default();
    assert_eq!(read_source(&file, Some(&control)).unwrap().len(), 100_000);
    // Reading is the first half of the work.
    assert_eq!(control.done.load(Relaxed), 100_000);
    assert_eq!(control.total.load(Relaxed), 200_000);

    control.cancel.store(true, Relaxed);
    assert!(matches!(
        read_source(&file, Some(&control)),
        Err(ImportError::Cancelled)
    ));
}
