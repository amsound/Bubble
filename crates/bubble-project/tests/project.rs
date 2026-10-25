use std::fs;
use std::path::{Path, PathBuf};

use bubble_project::fingerprint::Match;
use bubble_project::io::{backup_path, from_json_str, to_json_string};
use bubble_project::resolve::{Via, relative_path};
use bubble_project::*;
use proptest::prelude::*;

fn fixture() -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/example.bubble"),
    )
    .unwrap()
}

fn tmpdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn spec_example_parses_validates_and_round_trips() {
    let p = from_json_str(&fixture()).unwrap();
    assert_eq!(p.name, "Morning Show");
    assert_eq!(p.pages.len(), 2);
    assert_eq!(p.carts().count(), 6);
    assert_eq!(p.bindings.len(), 6);
    let bed = p.carts().map(|(_, c)| &c.cart).find(|c| c.looping).unwrap();
    assert_eq!(bed.label, "News Bed (loop)");
    assert_eq!(bed.retrigger, Some(Retrigger::ToggleStop));
    // Round trip is semantically identical.
    let again = from_json_str(&to_json_string(&p)).unwrap();
    assert_eq!(p, again);
}

#[test]
fn unknown_fields_are_preserved() {
    let mut v: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    v["future_top_level"] = serde_json::json!({"x": 1});
    v["pages"][0]["cells"][0]["cart"]["future_cart_field"] = serde_json::json!("keep me");
    let p = from_json_str(&v.to_string()).unwrap();
    let out: serde_json::Value = serde_json::from_str(&to_json_string(&p)).unwrap();
    assert_eq!(out["future_top_level"]["x"], 1);
    assert_eq!(
        out["pages"][0]["cells"][0]["cart"]["future_cart_field"],
        "keep me"
    );
}

#[test]
fn invalid_projects_are_rejected_with_precise_paths() {
    let mut v: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    v["pages"][0]["cells"][0]["cart"]["gain_db"] = serde_json::json!(40.0);
    v["pages"][0]["cells"][1]["cart"]["asset"] =
        serde_json::json!("00000000-0000-4000-8000-000000000999");
    v["bindings"][0]["targets"] = serde_json::json!(["00000000-0000-4000-8000-000000000998"]);
    match from_json_str(&v.to_string()) {
        Err(LoadError::Invalid(issues)) => {
            let paths: Vec<_> = issues.iter().map(|i| i.path.as_str()).collect();
            assert!(
                paths.contains(&"pages[0].cells[0].cart.gain_db"),
                "{paths:?}"
            );
            assert!(paths.contains(&"pages[0].cells[1].cart.asset"), "{paths:?}");
            assert!(paths.contains(&"bindings[0].targets"), "{paths:?}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn syntax_errors_newer_versions_and_foreign_json_are_rejected() {
    assert!(matches!(
        from_json_str("{ not json"),
        Err(LoadError::Syntax { .. })
    ));
    assert!(matches!(
        from_json_str(r#"{"hello":1}"#),
        Err(LoadError::NotAProject)
    ));
    let mut v: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    v["format_version"] = serde_json::json!(99);
    assert!(matches!(
        from_json_str(&v.to_string()),
        Err(LoadError::TooNew(99))
    ));
    let mut v: serde_json::Value = serde_json::from_str(&fixture()).unwrap();
    v["pages"][0]["cells"][0]["row"] = serde_json::json!(0);
    v["pages"][0]["cells"][0]["col"] = serde_json::json!(1); // collides with cell 1
    assert!(matches!(
        from_json_str(&v.to_string()),
        Err(LoadError::Invalid(_))
    ));
}

#[test]
fn save_is_atomic_and_keeps_backup() {
    let dir = tmpdir();
    let path = dir.path().join("Show.bubble");
    let mut p = Project::new("Show");
    save(&mut p, &path).unwrap();
    let first = fs::read_to_string(&path).unwrap();
    p.name = "Show v2".into();
    save(&mut p, &path).unwrap();
    assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), first);
    assert_eq!(load(&path).unwrap().name, "Show v2");
    // No temp files left behind.
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with(".bubble-save-")),
        "{names:?}"
    );
}

#[test]
fn invalid_project_is_never_written() {
    let dir = tmpdir();
    let path = dir.path().join("Bad.bubble");
    let mut p = Project::new("Ok");
    save(&mut p, &path).unwrap();
    p.pages.clear();
    assert!(matches!(save(&mut p, &path), Err(SaveError::Invalid(_))));
    assert_eq!(load(&path).unwrap().pages.len(), 1, "previous file intact");
}

#[test]
fn relative_paths_are_portable() {
    let base = Path::new("/shows/Morning");
    assert_eq!(
        relative_path(Path::new("/shows/Morning/audio/a.wav"), base).as_deref(),
        Some("audio/a.wav")
    );
    assert_eq!(
        relative_path(Path::new("/shows/Shared/b.wav"), base).as_deref(),
        Some("../Shared/b.wav")
    );
}

fn project_with_asset(dir: &Path, rel: &str) -> (Project, uuid::Uuid) {
    let file = dir.join(rel);
    let mut p = Project::new("T");
    let a = make_asset_ref(Some(dir), &file).unwrap();
    let id = a.id;
    p.assets.push(a);
    p.pages[0].ensure_cart(0, 0).asset = Some(id);
    (p, id)
}

#[test]
fn resolves_relative_path_after_moving_the_whole_project() {
    let dir = tmpdir();
    let proj = dir.path().join("Show");
    write(&proj.join("audio/a.wav"), b"RIFF-a");
    let (p, id) = project_with_asset(&proj, "audio/a.wav");
    assert_eq!(p.asset(id).unwrap().path, "audio/a.wav");
    // Move the project folder.
    let moved = dir.path().join("Elsewhere/Show");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(&proj, &moved).unwrap();
    let mut r = Resolver::new(Some(&moved), &p);
    match r.resolve(p.asset(id).unwrap()) {
        Resolution::Found { matched, via, .. } => {
            assert_eq!(matched, Match::Exact);
            assert_eq!(via, Via::StoredPath);
        }
        m => panic!("{m:?}"),
    }
}

#[test]
fn search_paths_find_moved_file_only_if_fingerprint_matches() {
    let dir = tmpdir();
    let proj = dir.path().join("Show");
    write(&proj.join("a.wav"), b"original content");
    let (mut p, id) = project_with_asset(&proj, "a.wav");
    p.asset_mut(id).unwrap().absolute_hint = None;
    fs::remove_file(proj.join("a.wav")).unwrap();
    // A different file with the same name elsewhere must not be accepted…
    write(&dir.path().join("Lib/x/a.wav"), b"different content");
    // …but the real file, moved deeper, must be.
    write(&dir.path().join("Lib/y/z/a.wav"), b"original content");
    p.asset_search_paths = vec![".".into(), "../Lib".into()];
    let mut r = Resolver::new(Some(&proj), &p);
    match r.resolve(p.asset(id).unwrap()) {
        Resolution::Found { path, matched, via } => {
            assert!(path.ends_with("Lib/y/z/a.wav"), "{path:?}");
            assert_eq!((matched, via), (Match::Exact, Via::Search));
        }
        m => panic!("{m:?}"),
    }
}

#[test]
fn edited_file_is_reported_changed() {
    let dir = tmpdir();
    write(&dir.path().join("a.wav"), b"one");
    let (p, id) = project_with_asset(dir.path(), "a.wav");
    fs::write(dir.path().join("a.wav"), b"two!").unwrap();
    let mut r = Resolver::new(Some(dir.path()), &p);
    assert!(matches!(
        r.resolve(p.asset(id).unwrap()),
        Resolution::Found {
            matched: Match::Changed,
            ..
        }
    ));
}

#[test]
fn relinking_one_file_relinks_the_rest_of_the_moved_folder() {
    let dir = tmpdir();
    let proj = dir.path().join("Show");
    let old = dir.path().join("Audio");
    for n in ["a.wav", "b.wav", "sub/c.wav"] {
        write(&old.join(n), n.as_bytes());
    }
    let mut p = Project::new("T");
    let mut ids = Vec::new();
    for (i, n) in ["a.wav", "b.wav", "sub/c.wav"].iter().enumerate() {
        let mut a = make_asset_ref(Some(&proj), &old.join(n)).unwrap();
        a.absolute_hint = None;
        ids.push(a.id);
        p.pages[0].ensure_cart(0, i as u16).asset = Some(a.id);
        p.assets.push(a);
    }
    // Move the audio folder somewhere else.
    let new = dir.path().join("Moved/AudioRenamed");
    fs::create_dir_all(new.parent().unwrap()).unwrap();
    fs::rename(&old, &new).unwrap();
    let mut r = Resolver::new(Some(&proj), &p);
    assert!(
        ids.iter()
            .all(|id| matches!(r.resolve(p.asset(*id).unwrap()), Resolution::Missing { .. }))
    );

    let report = relink(&mut p, Some(&proj), ids[0], &new.join("a.wav")).unwrap();
    assert_eq!(report.relinked.len(), 3, "{report:?}");
    let mut r = Resolver::new(Some(&proj), &p);
    for id in &ids {
        assert!(matches!(
            r.resolve(p.asset(*id).unwrap()),
            Resolution::Found {
                matched: Match::Exact,
                ..
            }
        ));
    }
    assert_eq!(
        p.asset(ids[2]).unwrap().path,
        "../Moved/AudioRenamed/sub/c.wav"
    );
}

proptest! {
    #[test]
    fn generated_projects_round_trip(
        name in "[a-zA-Z0-9 ]{1,30}",
        rows in 1u16..=8, cols in 1u16..=8,
        gains in proptest::collection::vec(-60.0f32..12.0, 0..20),
        loops in proptest::collection::vec(any::<bool>(), 20),
    ) {
        let mut p = Project::new(name);
        p.pages[0].rows = rows;
        p.pages[0].cols = cols;
        for (i, g) in gains.iter().enumerate() {
            let (r, c) = ((i as u16 / cols) % rows, i as u16 % cols);
            let cart = p.pages[0].ensure_cart(r, c);
            cart.gain_db = *g;
            cart.looping = loops[i];
            cart.label = format!("Cart {i}");
        }
        let text = to_json_string(&p);
        let back = from_json_str(&text).unwrap();
        prop_assert_eq!(p, back);
    }
}

#[test]
fn cell_numbers_count_on_across_pages() {
    let mut p = Project::new("x");
    p.pages.push(Page::new("Page 2", 3, 5));
    p.pages.push(Page::new("Page 3", 2, 2));
    assert_eq!(p.cell_number(0, 0, 0), 1);
    assert_eq!(p.cell_number(0, 3, 5), 24);
    assert_eq!(p.cell_number(1, 0, 0), 25);
    assert_eq!(p.cell_number(1, 2, 4), 39);
    assert_eq!(p.cell_number(2, 1, 1), 43);
}

#[test]
fn settings_moved_to_the_machine_survive_in_extra() {
    let json = r#"{"default_fade_out_ms": 100, "master_gain_db": 0.0, "limiter": false,
                   "mouse_mode": "left_play_right_stop", "numbering": "per_page"}"#;
    let s: bubble_project::ProjectSettings = serde_json::from_str(json).unwrap();
    assert_eq!(s.extra["master_gain_db"], 0.0);
    assert_eq!(s.extra["limiter"], false);
    let back = serde_json::to_value(&s).unwrap();
    assert_eq!(back["mouse_mode"], "left_play_right_stop");
}
