//! Importer tests on two sessions saved by SpotOn (`tests/fixtures/*.dta`), whose
//! button 2 refers to `Y:\…\TestFiles\1kHz_16_44_AB_M18.wav`. The fixtures and
//! `TestFiles/` are not distributed with the source; tests that need them skip
//! when they are absent.

use std::path::{Path, PathBuf};

use bubble_project::{
    Action, BindingSource, Curve, MidiChannel, MidiMessage, StopScope, Trim, validate,
};
use dta_import::convert::convert;
use dta_import::dta::{self, HotKey, MidiMsg, TColor};
use dta_import::report::CartStatus;
use dta_import::{DriveMap, ImportError, ImportOptions, SourceInfo, import_bytes, import_file};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The two real sessions saved by the reference app (tests/fixtures).
fn samples() -> Vec<PathBuf> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    vec![base.join("session.dta"), base.join("session-backup.dta")]
}

/// Button 2's file as stored in the fixture (a Windows path on drive Y:).
fn button2_path() -> String {
    let s = dta::parse(&read(&samples()[0])).unwrap();
    let p = s
        .carts
        .iter()
        .find(|c| c.is_used())
        .unwrap()
        .original_local_filename
        .clone();
    assert!(p.starts_with(r"Y:\") && p.ends_with(r"\TestFiles\1kHz_16_44_AB_M18.wav"));
    p
}

/// The same path below the drive, with forward slashes.
fn button2_rel() -> String {
    button2_path()[3..].replace('\\', "/")
}

/// Whether the SpotOn fixtures and `TestFiles/` are present (not distributed).
fn missing_fixtures() -> bool {
    let missing = !samples().iter().all(|p| p.exists()) || !repo().join("TestFiles").is_dir();
    if missing {
        eprintln!("skipped: SpotOn fixtures or TestFiles/ not present");
    }
    missing
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

#[test]
fn parses_real_sessions_exactly() {
    if missing_fixtures() {
        return;
    }
    for f in samples() {
        let s = dta::parse(&read(&f)).unwrap();
        assert_eq!(s.format_version, 99);
        assert!(s.trailer_ok, "{}", f.display());
        assert_eq!(s.page_names, ["1", "2", "3", "4", "5", "6", "7", "8"]);
        assert_eq!((s.grid_columns, s.grid_rows), (5, 3));
        assert_eq!(s.gain_offset, 10);
        assert!(s.session_folder.starts_with(r"Y:\"));
        assert_eq!(s.options.fade_out_default, 1);
        assert_eq!(s.options.link_names.len(), 256);
        assert_eq!(s.options.network_target, "127.0.0.1");
        assert_eq!(s.carts.len(), 320);
        let mut idx: Vec<u16> = s.carts.iter().map(|c| c.index).collect();
        idx.sort();
        assert_eq!(idx, (0..320).collect::<Vec<_>>());

        let used: Vec<_> = s.carts.iter().filter(|c| c.is_used()).collect();
        assert_eq!(used.len(), 1);
        let c = used[0];
        assert_eq!(c.button(), 2);
        assert_eq!(c.filename, "");
        assert_eq!(c.original_local_filename, button2_path());
        assert_eq!(c.source_path(), Some(button2_path().as_str()));
        assert_eq!(c.avg_bytes_per_sec, Some(176_400));
        assert_eq!((c.trim_start_bytes, c.trim_end_bytes), (0, 3_528_000));
        assert_eq!((c.fade_in_ds, c.fade_out_ds), (0, 1));
        assert_eq!((c.fade_in_law, c.fade_out_law), (1, 1));
        assert_eq!(c.panel_colour.to_hex().as_deref(), Some("#3A6EA5"));
        assert_eq!(c.output_device_guid, "9B1C6C41-779A-4AE9-918C-10B3F2E74044");
        assert!(!c.midi_play_in.is_valid() && !c.midi_stop_in.is_valid());
        assert_eq!(c.midi_play_in.0, [1, 0, 0, 0, 0, 0x80, 0, 0]);
        assert!(!c.hotkey.is_set());
        assert_eq!((c.play_group, c.preset_gain_db, c.pan), (0, 0, 0.0));
    }
}

#[test]
fn truncated_session_is_rejected() {
    if missing_fixtures() {
        return;
    }
    for f in samples() {
        let d = read(&f);
        assert!(dta::parse(&d[..d.len() - 100]).is_err());
        let mut bad = d.clone();
        let n = bad.len();
        bad[n - 1] ^= 0xff; // integrity token
        let s = dta::parse(&bad).unwrap();
        assert!(!s.trailer_ok);
        assert!(matches!(
            import_bytes(&bad, "x.dta", None, &ImportOptions::default(), false),
            Err(ImportError::Trailer)
        ));
    }
}

#[test]
fn missing_asset_is_reported_with_its_original_path() {
    if missing_fixtures() {
        return;
    }
    for f in samples() {
        let dir = tempfile::tempdir().unwrap();
        let mut drives = DriveMap::default();
        drives
            .add_spec(&format!("Y:={}", dir.path().display()))
            .unwrap();
        let opts = ImportOptions {
            drives,
            ..Default::default()
        };
        let (project, report) =
            import_bytes(&read(&f), "~temp.dta", Some(dir.path()), &opts, false).unwrap();
        assert!(validate(&project).is_empty(), "{:?}", validate(&project));

        assert_eq!(report.source.format_version, 99);
        assert!(report.source.trailer_ok);
        assert_eq!(report.source.sha256.len(), 64);
        assert_eq!(report.summary.slots_total, 320);
        assert_eq!(report.summary.carts_imported, 1);
        assert_eq!(
            (
                report.summary.assets_resolved,
                report.summary.assets_missing
            ),
            (0, 1)
        );
        assert!(
            report
                .manual_actions
                .iter()
                .any(|a| a.starts_with("Relink 1"))
        );

        let c = &report.carts[0];
        assert_eq!(c.ref_button, 2);
        assert_eq!(c.status, CartStatus::ImportedMissingAsset);
        assert_eq!((c.page.as_str(), c.row, c.col), ("1", 0, 1));
        assert_eq!(c.label, "1kHz_16_44_AB_M18");
        let a = c.asset.as_ref().unwrap();
        assert_eq!(a.ref_path, button2_path());
        assert_eq!(a.resolved, None);
        let mapped = dir.path().join(button2_rel());
        assert_eq!(a.tried, [mapped.display().to_string()]);

        // JSON shape of the migration report.
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["carts"][0]["status"], "imported_missing_asset");
        assert_eq!(json["carts"][0]["asset"]["ref_path"], button2_path());
        assert!(json["carts"][0]["asset"]["resolved"].is_null());
        assert_eq!(json["summary"]["assets_missing"], 1);

        // Project: layout, page names, cart settings, asset kept as a Windows hint.
        assert_eq!(project.pages.len(), 8);
        assert!(project.pages.iter().all(|p| (p.rows, p.cols) == (3, 5)));
        assert_eq!(project.pages[7].name, "8");
        assert_eq!(project.settings.default_fade_out_ms, 100);
        let cell = &project.pages[0].cells[0];
        assert_eq!((cell.row, cell.col), (0, 1));
        let cart = &cell.cart;
        assert_eq!(cart.id, c.new_cart_id);
        assert_eq!(cart.color.as_deref(), Some("#3A6EA5"));
        // The reference's device index 1 (its only device) is Output A.
        assert_eq!(cart.output, "A");
        assert_eq!(cart.gain_db, 0.0);
        assert_eq!(cart.fade_out.unwrap().ms, 100);
        assert_eq!(cart.fade_out.unwrap().curve, Curve::Exponential);
        assert_eq!(cart.fade_in, None);
        // 3,528,000 bytes at 176,400 bytes/s, 16-bit stereo = 882,000 frames (20 s).
        assert_eq!(
            cart.trim,
            Some(Trim {
                in_frames: 0,
                out_frames: 882_000
            })
        );
        let asset = project.asset(cart.asset.unwrap()).unwrap();
        assert_eq!(asset.path, button2_rel());
        let hint = asset.absolute_hint.as_ref().unwrap();
        assert_eq!(hint.path, button2_path());
        assert_eq!(hint.platform, bubble_project::Platform::Windows);
        assert!(asset.fingerprint.is_none());
    }
}

#[test]
fn asset_found_through_search_root_and_drive_map() {
    if missing_fixtures() {
        return;
    }
    for f in samples() {
        let out = tempfile::tempdir().unwrap();

        // --search <repo>: the path's tail TestFiles/1kHz… exists there.
        let opts = ImportOptions {
            search_roots: vec![repo()],
            ..Default::default()
        };
        let (project, report) =
            import_bytes(&read(&f), "~temp.dta", Some(out.path()), &opts, false).unwrap();
        assert!(validate(&project).is_empty());
        let c = &report.carts[0];
        assert_eq!(c.status, CartStatus::Imported);
        let a = c.asset.as_ref().unwrap();
        assert_eq!(a.via.as_deref(), Some("search_root"));
        assert!(
            a.resolved
                .as_ref()
                .unwrap()
                .ends_with("TestFiles/1kHz_16_44_AB_M18.wav")
        );
        assert_eq!(
            (
                report.summary.assets_resolved,
                report.summary.assets_missing
            ),
            (1, 0)
        );
        let cart = &project.pages[0].cells[0].cart;
        // Untrimmed in the reference (out = data length), so no trim in the project.
        assert_eq!(cart.trim, None);
        let asset = project.asset(cart.asset.unwrap()).unwrap();
        assert!(asset.fingerprint.is_some());
        let meta = asset.meta.as_ref().unwrap();
        assert_eq!(
            (meta.sample_rate, meta.channels, meta.frames),
            (44100, 2, 882_000)
        );

        // --map Y:=<dir> with the file at the mapped location.
        let y = tempfile::tempdir().unwrap();
        let dest = y.path().join(Path::new(&button2_rel()).parent().unwrap());
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(
            repo().join("TestFiles/1kHz_16_44_AB_M18.wav"),
            dest.join("1kHz_16_44_AB_M18.wav"),
        )
        .unwrap();
        let mut drives = DriveMap::default();
        drives
            .add_spec(&format!("Y:={}", y.path().display()))
            .unwrap();
        let opts = ImportOptions {
            drives,
            ..Default::default()
        };
        let (_, report) =
            import_bytes(&read(&f), "~temp.dta", Some(out.path()), &opts, false).unwrap();
        assert_eq!(report.carts[0].status, CartStatus::Imported);
        assert_eq!(
            report.carts[0].asset.as_ref().unwrap().via.as_deref(),
            Some("drive_map")
        );
    }
}

#[test]
fn import_file_writes_project_and_reports_and_leaves_source_alone() {
    if missing_fixtures() {
        return;
    }
    for f in samples() {
        let before = read(&f);
        let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("Show.bubble");
        let opts = ImportOptions::default();

        let o = import_file(&f, &out, &opts, false).unwrap();
        assert_eq!(read(&f), before);
        assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), mtime);

        let loaded = bubble_project::load(&out).unwrap();
        assert_eq!(loaded.name, f.file_stem().unwrap().to_str().unwrap());
        assert_eq!(loaded.pages.len(), 8);
        assert_eq!(o.report_json, dir.path().join("Show.import-report.json"));
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&o.report_json).unwrap()).unwrap();
        assert_eq!(json["carts"][0]["ref_button"], 2);
        let md = std::fs::read_to_string(&o.report_md).unwrap();
        assert!(md.contains("imported_missing_asset"));
        assert!(md.contains("1kHz_16_44_AB_M18.wav"));

        assert!(matches!(
            import_file(&f, &out, &opts, false),
            Err(ImportError::OutputExists(_))
        ));
        import_file(&f, &out, &opts, true).unwrap();
        assert!(matches!(
            import_file(&f, &f, &opts, true),
            Err(ImportError::OutputIsSource)
        ));
        assert!(matches!(
            import_file(&f, &dir.path().join("x.dta"), &opts, true),
            Err(ImportError::OutputIsSource)
        ));
        assert_eq!(read(&f), before);
    }
}

/// Non-default values, set on the parsed real session (the real files only carry
/// defaults), exercise every mapping rule.
#[test]
fn maps_non_default_cart_settings() {
    if missing_fixtures() {
        return;
    }
    let Some(f) = samples().into_iter().next() else {
        return;
    };
    let mut s = dta::parse(&read(&f)).unwrap();
    let i2 = s.carts.iter().position(|c| c.index == 1).unwrap();
    {
        let c = &mut s.carts[i2];
        c.track_name = "Intro".into();
        c.comment = "first\r\nsecond".into();
        c.panel_colour = TColor(0x0000FF); // BGR red
        c.preset_gain_db = 4;
        c.pan = -0.5;
        c.looped = true;
        c.trim_start_bytes = 176_400; // 1 s
        c.trim_end_bytes = 176_400 * 3;
        c.fade_in_ds = 5;
        c.fade_in_law = 0;
        c.fade_out_ds = 20;
        c.stop_all_on_play = true;
        c.play_next = true;
        c.play_group = 3;
        c.mouse_up_stop = true;
        c.hotkey = HotKey {
            vk: 0x70,
            flag: 0,
            shift: 0b101,
        }; // Ctrl+Shift+F1
        c.midi_play_in = MidiMsg([0, 0, 0, 0, 0x92, 60, 100, 0]); // note 60, channel 3
        c.midi_stop_in = MidiMsg([1, 0, 0, 0, 0xB0, 20, 127, 0]); // CC 20, any channel
        c.active_effects = 1;
        c.stop_row_on_play = true;
    }
    // Button 3: same file, same hot-key → one ganged binding; page-scoped stop.
    let mut c3 = s.carts[i2].clone();
    c3.index = 2;
    c3.stop_all_on_play = false;
    c3.stop_page_on_play = true;
    c3.play_group = 0;
    c3.midi_play_in = MidiMsg([1, 0, 0, 0, 0, 0x80, 0, 0]);
    c3.midi_stop_in = c3.midi_play_in;
    c3.panel_colour = TColor(0x8000_0005u32 as i32); // system colour
    // Button 200: on hidden page 14 of the 5 × 3 layout.
    let mut c200 = c3.clone();
    c200.index = 199;
    c200.hotkey = HotKey::default();
    let i3 = s.carts.iter().position(|c| c.index == 2).unwrap();
    let i200 = s.carts.iter().position(|c| c.index == 199).unwrap();
    s.carts[i3] = c3;
    s.carts[i200] = c200;

    let out = tempfile::tempdir().unwrap();
    let opts = ImportOptions {
        search_roots: vec![repo().join("TestFiles")],
        ..Default::default()
    };
    let src = SourceInfo {
        file: "show.dta".into(),
        sha256: String::new(),
    };
    let (project, report) = convert(&s, &src, Some(out.path()), &opts).unwrap();
    assert!(validate(&project).is_empty(), "{:?}", validate(&project));
    assert_eq!(project.name, "show");
    assert_eq!(report.summary.carts_imported, 3);
    assert_eq!(
        project.assets.len(),
        1,
        "carts sharing a file share one asset"
    );
    assert_eq!(project.pages.len(), 14);
    assert_eq!(project.pages[13].name, "Page 14");
    assert_eq!(project.pages[13].cells[0].row, 0);
    assert_eq!(project.pages[13].cells[0].col, 4);

    let r2 = report.carts.iter().find(|r| r.ref_button == 2).unwrap();
    let cart = project.cart(r2.new_cart_id).unwrap();
    assert_eq!(cart.label, "Intro");
    assert_eq!(cart.notes, "first\r\nsecond");
    assert_eq!(cart.color.as_deref(), Some("#FF0000"));
    assert_eq!((cart.gain_db, cart.pan), (4.0, -0.5));
    assert!(cart.looping && cart.hold_to_play);
    assert_eq!(
        cart.trim,
        Some(Trim {
            in_frames: 44_100,
            out_frames: 132_300
        })
    );
    let fi = cart.fade_in.unwrap();
    assert_eq!((fi.ms, fi.curve), (500, Curve::Linear));
    assert_eq!(cart.fade_out.unwrap().ms, 2000);
    assert_eq!(cart.on_play.stop, StopScope::All);
    assert_eq!(cart.play_next.as_deref(), Some("next_cell"));
    assert_eq!(
        cart.group,
        Some(project.groups[2].id),
        "reference group 3 = G3"
    );
    assert!(r2.unsupported.iter().any(|u| u.field == "effects"));
    assert!(r2.unsupported.iter().any(|u| u.field == "stop_row_on_play"));
    assert!(r2.warnings.iter().any(|w| w.contains("not yet confirmed")));
    assert!(
        report
            .discarded_features
            .iter()
            .any(|f| f.starts_with("DirectSound FX"))
    );

    let r3 = report.carts.iter().find(|r| r.ref_button == 3).unwrap();
    let c3 = project.cart(r3.new_cart_id).unwrap();
    assert_eq!(c3.on_play.stop, StopScope::Page);
    assert_eq!(c3.color, None);
    assert!(r3.unsupported.iter().any(|u| u.field == "panel_colour"));
    assert_eq!((r3.row, r3.col), (0, 2));

    let key = project
        .bindings
        .iter()
        .find(|b| matches!(b.source, BindingSource::Key { .. }))
        .unwrap();
    assert_eq!(
        key.source,
        BindingSource::Key {
            key: "F1".into(),
            modifiers: vec!["Ctrl".into(), "Shift".into()]
        }
    );
    assert_eq!(key.action, Action::Play);
    assert_eq!(key.targets, vec![r2.new_cart_id, r3.new_cart_id]);

    let note = project
        .bindings
        .iter()
        .find(|b| {
            matches!(
                b.source,
                BindingSource::Midi {
                    message: MidiMessage::NoteOn,
                    ..
                }
            )
        })
        .unwrap();
    assert_eq!(
        (note.action, &note.targets),
        (Action::Play, &vec![r2.new_cart_id])
    );
    assert!(matches!(
        note.source,
        BindingSource::Midi {
            channel: MidiChannel(Some(3)),
            number: 60,
            ..
        }
    ));
    let cc = project
        .bindings
        .iter()
        .find(|b| {
            matches!(
                b.source,
                BindingSource::Midi {
                    message: MidiMessage::Cc,
                    ..
                }
            )
        })
        .unwrap();
    assert_eq!(cc.action, Action::Stop);
    assert!(matches!(
        cc.source,
        BindingSource::Midi {
            channel: MidiChannel(None),
            number: 20,
            threshold: Some(64),
            ..
        }
    ));
    assert_eq!(project.bindings.len(), 3);
}
