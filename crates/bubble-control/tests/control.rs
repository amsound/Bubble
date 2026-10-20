//! Controller tests with an offline engine (no audio device).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bubble_control::*;
use bubble_engine::{Engine, EngineConfig, Mixer};
use bubble_project::{Action, BindingSource, MidiChannel, MidiMessage, StopScope, Trim};
use uuid::Uuid;

const SR: u32 = 48_000;

/// Whether `TestFiles/` (reference test signals, not distributed with the source) is
/// absent; tests that need it skip.
fn missing_test_files() -> bool {
    let missing = !PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../TestFiles")
        .is_dir();
    if missing {
        eprintln!("skipped: TestFiles/ not present");
    }
    missing
}

fn test_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../TestFiles")
        .join(name)
}

fn setup() -> (Controller, Mixer) {
    let (h, m) = Engine::new(EngineConfig::new(SR, 2));
    let mut c = Controller::new(Controller::new_project("Test"), None);
    c.attach_engine(h);
    (c, m)
}

fn wait_ready(c: &mut Controller) {
    let t = Instant::now();
    while c.assets_pending() > 0 {
        c.tick();
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "assets never finished loading"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    c.tick();
}

/// Advances the engine by `frames` and processes events.
fn run(c: &mut Controller, m: &mut Mixer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * 2];
    m.process(&mut out);
    c.tick();
    out
}

fn state(c: &Controller, cart: Uuid) -> CartState {
    c.runtime()[&cart].state
}

fn key(code: &str, pressed: bool, repeat: bool) -> InputEvent {
    InputEvent::Key {
        code: code.into(),
        modifiers: vec![],
        pressed,
        repeat,
    }
}

fn note_on(n: u8) -> InputEvent {
    InputEvent::Midi {
        port: "test".into(),
        event: MidiEvent::parse(&[0x90, n, 100]).unwrap(),
    }
}

#[test]
fn assign_play_and_stop_by_mouse() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    assert_eq!(c.project().cart(cart).unwrap().label, "1kHz_16_48_AB_M18");
    wait_ready(&mut c);
    assert_eq!(state(&c, cart), CartState::Ready);
    // Asset metadata filled from the decoder.
    let meta = c.project().assets[0].meta.clone().unwrap();
    assert_eq!((meta.sample_rate, meta.channels), (48_000, 2));

    c.pointer_down(cart, MouseButton::Primary);
    let out = run(&mut c, &mut m, 4800);
    assert_eq!(state(&c, cart), CartState::Playing);
    let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    assert!(
        (20.0 * peak.log10() + 18.0).abs() < 0.2,
        "plays at −18 dBFS"
    );

    // Toggle mode: second click stops (with the default 100 ms fade).
    c.pointer_down(cart, MouseButton::Primary);
    run(&mut c, &mut m, 480);
    assert_eq!(state(&c, cart), CartState::Stopping);
    run(&mut c, &mut m, 9600);
    assert_eq!(state(&c, cart), CartState::Ready);
    assert!(
        c.log()
            .iter()
            .any(|e| e.kind == LogKind::Started(TriggerSource::Mouse))
    );
}

#[test]
fn gain_edit_applies_live_and_persists() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_0.wav"))
        .unwrap();
    wait_ready(&mut c);
    c.trigger(cart, TriggerSource::Ui, None);
    run(&mut c, &mut m, 960);
    c.edit_cart(cart, |x| x.gain_db = -6.0);
    let out = run(&mut c, &mut m, 4800);
    let tail = &out[out.len() / 2..];
    let peak = tail.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    // Cart −6 dB plus the default −10 dB global output attenuation.
    assert!(
        (20.0 * peak.log10() + 16.0).abs() < 0.3,
        "{} dB",
        20.0 * peak.log10()
    );
    assert!(c.is_dirty());
}

#[test]
fn key_learn_trigger_ignores_repeat_and_escape_stops_all() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    wait_ready(&mut c);
    c.start_learn(cart, Action::Play);
    assert!(c.handle_input(&key("F1", true, false)));
    assert!(c.learning().is_none());
    assert_eq!(c.bindings_for(cart).len(), 1);

    assert!(c.handle_input(&key("F1", true, false)));
    run(&mut c, &mut m, 480);
    assert_eq!(state(&c, cart), CartState::Playing);
    let started = |c: &Controller| {
        c.log()
            .iter()
            .filter(|e| e.kind == LogKind::Started(TriggerSource::Key))
            .count()
    };
    assert_eq!(started(&c), 1);
    // Auto-repeat does not retrigger.
    c.handle_input(&key("F1", true, true));
    assert_eq!(started(&c), 1);

    // Default project binding: Escape = stop all.
    assert!(c.handle_input(&key("Escape", true, false)));
    run(&mut c, &mut m, 9600);
    assert_eq!(state(&c, cart), CartState::Ready);
}

#[test]
fn midi_ganged_and_overlapped_bindings() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let a = c
        .assign_file(0, 0, 0, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    let b = c
        .assign_file(0, 0, 1, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    assert_eq!(c.project().assets.len(), 1, "same file shares one asset");
    wait_ready(&mut c);
    let src = BindingSource::Midi {
        device: "*".into(),
        channel: MidiChannel(Some(1)),
        message: MidiMessage::NoteOn,
        number: 60,
        threshold: None,
    };
    c.set_binding(a, src.clone(), Action::Play);
    c.set_binding(b, src.clone(), Action::Play);
    assert_eq!(c.duplicates_of(&src, a), vec![b]);

    // Ganged (default): both play.
    c.handle_input(&note_on(60));
    run(&mut c, &mut m, 480);
    assert_eq!(
        (state(&c, a), state(&c, b)),
        (CartState::Playing, CartState::Playing)
    );
    c.stop_all(false);
    run(&mut c, &mut m, 480);

    // Overlapped: one per trigger, round-robin.
    c.edit_settings(|s| s.duplicate_binding_policy = bubble_project::DuplicatePolicy::Overlapped);
    c.handle_input(&note_on(60));
    run(&mut c, &mut m, 480);
    assert_eq!(
        (state(&c, a), state(&c, b)),
        (CartState::Playing, CartState::Ready)
    );
    c.handle_input(&note_on(60));
    run(&mut c, &mut m, 480);
    assert_eq!(
        (state(&c, a), state(&c, b)),
        (CartState::Playing, CartState::Playing)
    );
    // Wrong channel or note: no match.
    let other = InputEvent::Midi {
        port: "test".into(),
        event: MidiEvent::parse(&[0x91, 60, 100]).unwrap(),
    };
    assert!(!c.handle_input(&other));
}

#[test]
fn midi_parse_and_velocity_zero_note_off() {
    let e = MidiEvent::parse(&[0x93, 64, 0]).unwrap();
    assert_eq!(
        (e.channel, e.message, e.number),
        (4, MidiMessage::NoteOff, 64)
    );
    assert!(MidiEvent::parse(&[0xF8]).is_none(), "clock ignored");
    assert!(
        MidiEvent::parse(&[0x40]).is_none(),
        "running status without status byte"
    );
}

#[test]
fn exclusive_group_and_stop_all_on_play() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let a = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let b = c
        .assign_file(0, 0, 1, &test_file("1kHz_16_48_AB_M20.wav"))
        .unwrap();
    let d = c
        .assign_file(0, 0, 2, &test_file("1kHz_16_48_AB_M24.wav"))
        .unwrap();
    wait_ready(&mut c);
    let g = c.ensure_group("Beds");
    c.edit_cart(a, |x| x.group = Some(g));
    c.edit_cart(b, |x| x.group = Some(g));
    c.trigger(a, TriggerSource::Ui, None);
    c.trigger(d, TriggerSource::Ui, None);
    run(&mut c, &mut m, 480);
    c.trigger(b, TriggerSource::Ui, None);
    run(&mut c, &mut m, 9600);
    assert_eq!(state(&c, a), CartState::Ready, "same group stopped");
    assert_eq!(state(&c, b), CartState::Playing);
    assert_eq!(state(&c, d), CartState::Playing, "other carts untouched");

    c.edit_cart(a, |x| {
        x.group = None;
        x.on_play.stop = StopScope::All;
    });
    c.trigger(a, TriggerSource::Ui, None);
    run(&mut c, &mut m, 9600);
    assert_eq!(
        (state(&c, a), state(&c, b), state(&c, d)),
        (CartState::Playing, CartState::Ready, CartState::Ready)
    );
}

#[test]
fn missing_file_is_rejected_then_relinked() {
    if missing_test_files() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("Show");
    let audio = dir.path().join("Audio");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&audio).unwrap();
    std::fs::copy(test_file("ding_16_48_AB.wav"), audio.join("ding.wav")).unwrap();
    std::fs::copy(test_file("ding_16_44_AB.wav"), audio.join("ding44.wav")).unwrap();
    let path = proj.join("Show.bubble");

    let (mut c, _m) = setup();
    c.save_as(&path).unwrap();
    let a = c.assign_file(0, 0, 0, &audio.join("ding.wav")).unwrap();
    let b = c.assign_file(0, 0, 1, &audio.join("ding44.wav")).unwrap();
    assert_eq!(
        c.project().assets[0].path,
        "../Audio/ding.wav",
        "stored relative"
    );
    c.save().unwrap();

    // Move the audio folder (outside the project's search paths): both carts go missing.
    std::fs::rename(&audio, dir.path().join("Sounds")).unwrap();
    let mut p = c.project().clone();
    p.assets.iter_mut().for_each(|a| a.absolute_hint = None);
    c.replace_project(p, Some(path.clone()));
    wait_ready(&mut c);
    assert_eq!(state(&c, a), CartState::Missing);
    assert!(!c.trigger(a, TriggerSource::Ui, None));
    assert!(matches!(c.log().back().unwrap().kind, LogKind::Rejected(_)));

    let missing = c.missing_assets();
    assert_eq!(missing.len(), 2);
    let n = c
        .relink(missing[0], &dir.path().join("Sounds/ding.wav"))
        .unwrap();
    assert_eq!(n, 2, "second file relinked automatically");
    wait_ready(&mut c);
    assert_eq!(
        (state(&c, a), state(&c, b)),
        (CartState::Ready, CartState::Ready)
    );
    assert_eq!(c.project().assets[1].path, "../Sounds/ding44.wav");
}

#[test]
fn trims_scale_with_source_rate_and_play_next_chains() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    // 44.1 kHz source in a 48 kHz engine.
    let a = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_44_AB_0.wav"))
        .unwrap();
    let b = c
        .assign_file(0, 0, 1, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    wait_ready(&mut c);
    c.edit_cart(a, |x| {
        x.trim = Some(Trim {
            in_frames: 0,
            out_frames: 4410,
        }); // 100 ms
        x.fade_out = Some(bubble_project::FadeSpec::ms(0));
        x.play_next = Some("next_cell".into());
    });
    c.trigger(a, TriggerSource::Ui, None);
    let rt = {
        run(&mut c, &mut m, 64);
        c.runtime()[&a]
    };
    assert!((rt.duration_s - 0.1).abs() < 0.001, "{}", rt.duration_s);
    run(&mut c, &mut m, 4800);
    run(&mut c, &mut m, 64);
    assert_eq!(state(&c, a), CartState::Ready);
    assert_eq!(
        state(&c, b),
        CartState::Playing,
        "play_next started the next cell"
    );
    assert!(
        c.log()
            .iter()
            .any(|e| e.kind == LogKind::Started(TriggerSource::PlayNext))
    );
}

#[test]
fn save_reopen_round_trip_keeps_carts_and_bindings() {
    if missing_test_files() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Round.bubble");
    let (mut c, _m) = setup();
    let cart = c
        .assign_file(0, 1, 2, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    c.edit_cart(cart, |x| {
        x.color = Some("#AA3300".into());
        x.looping = true;
    });
    c.set_binding(
        cart,
        BindingSource::Key {
            key: "KeyQ".into(),
            modifiers: vec![],
        },
        Action::Toggle,
    );
    c.add_page("Second", 2, 2);
    c.save_as(&path).unwrap();
    assert!(!c.is_dirty());

    let mut c2 = Controller::new(Controller::new_project("x"), None);
    c2.open(&path).unwrap();
    let p = c2.project();
    assert_eq!(p.pages.len(), 2);
    let x = p.cart(cart).unwrap();
    assert_eq!((x.color.as_deref(), x.looping), (Some("#AA3300"), true));
    assert_eq!(c2.bindings_for(cart)[0].action, Action::Toggle);
    assert_eq!(p.cart_position(cart), Some((0, 1, 2)));
    assert!(Path::new(&p.assets[0].path).is_relative());
}

#[test]
fn grid_edits_are_validated() {
    if missing_test_files() {
        return;
    }
    let (mut c, _m) = setup();
    c.assign_file(0, 3, 5, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    assert!(c.resize_page(0, 2, 2).is_err(), "cart would fall outside");
    assert!(c.resize_page(0, 4, 8).is_ok());
    assert!(c.remove_page(0).is_err(), "last page");
    c.move_cell((0, 3, 5), (0, 0, 0));
    assert!(c.project().pages[0].cell(0, 0).is_some());
    c.clear_cell(0, 0, 0);
    assert!(c.project().assets.is_empty(), "unused asset pruned");
}

#[test]
fn undo_redo_and_edit_coalescing() {
    if missing_test_files() {
        return;
    }
    let (mut c, _m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    // A slider drag: many edits of the same cart within a second = one undo step.
    for g in 1..=10 {
        c.edit_cart(cart, |x| x.gain_db = -(g as f32));
    }
    c.edit_cart(cart, |x| x.gain_db = -10.0); // no-op does not add a step
    assert_eq!(c.project().cart(cart).unwrap().gain_db, -10.0);
    assert!(c.undo());
    assert_eq!(
        c.project().cart(cart).unwrap().gain_db,
        0.0,
        "whole drag undone at once"
    );
    assert!(c.undo());
    assert!(c.project().cart(cart).is_none(), "assignment undone");
    assert!(c.project().assets.is_empty());
    assert!(!c.undo());
    assert!(c.redo());
    assert!(c.redo());
    assert_eq!(c.project().cart(cart).unwrap().gain_db, -10.0);
    assert!(!c.can_redo());
    // A new edit clears the redo stack.
    c.undo();
    c.edit_cart(cart, |x| x.looping = true);
    assert!(!c.can_redo());
}

#[test]
fn copy_paste_cart_and_paste_attributes() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let a = c
        .assign_file(0, 0, 0, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    let b = c
        .assign_file(0, 0, 1, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    c.edit_cart(a, |x| {
        x.gain_db = -4.0;
        x.color = Some("#123456".into());
        x.looping = true;
    });
    c.set_binding(
        a,
        BindingSource::Key {
            key: "KeyZ".into(),
            modifiers: vec![],
        },
        Action::Play,
    );
    let clip = c.copy_cart(a).unwrap();
    let pasted = c.paste_cart(0, 1, 0, &clip).unwrap();
    assert_ne!(pasted, a);
    let p = c.project().cart(pasted).unwrap();
    assert_eq!(
        (p.gain_db, p.looping, p.asset),
        (-4.0, true, Some(clip.asset.unwrap().id))
    );
    assert!(c.bindings_for(pasted).is_empty(), "bindings are not copied");
    assert_eq!(c.project().assets.len(), 2, "asset shared, not duplicated");
    wait_ready(&mut c);
    c.trigger(pasted, TriggerSource::Ui, None);
    run(&mut c, &mut m, 480);
    assert_eq!(state(&c, pasted), CartState::Playing);

    let from = c.project().cart(a).unwrap().clone();
    c.paste_attributes(
        &from,
        &[b],
        AttrSet {
            gain: true,
            colour: true,
            pan: false,
            fades: false,
            looping: false,
            behaviour: false,
        },
    );
    let bb = c.project().cart(b).unwrap();
    assert_eq!(
        (bb.gain_db, bb.color.as_deref(), bb.looping),
        (-4.0, Some("#123456"), false)
    );
    assert!(c.undo());
    assert_eq!(
        c.project().cart(b).unwrap().gain_db,
        0.0,
        "paste attributes is one undo step"
    );
}

#[test]
fn new_project_defaults_minus_10_db_and_eight_groups() {
    let p = Controller::new_project("x");
    assert_eq!(p.settings.master_gain_db, -10.0);
    assert_eq!(p.groups.len(), 8);
    assert_eq!(p.groups[0].label(0), "G1");
    let mut g = p.groups[2].clone();
    g.name = "Beds".into();
    assert_eq!(g.label(2), "G3 Beds");
}

#[test]
fn hotkey_on_looping_cart_toggles_and_stays_stopped() {
    if missing_test_files() {
        return;
    }
    // Reported bug: repeated hotkey presses on a looping cart left it playing.
    let (mut c, mut m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    c.edit_cart(cart, |x| x.looping = true);
    wait_ready(&mut c);
    let action = c.default_trigger_action();
    assert_eq!(action, Action::Toggle, "learned keys behave like a click");
    c.start_learn(cart, action);
    c.handle_input(&key("KeyL", true, false));
    for i in 0..6 {
        c.handle_input(&key("KeyL", true, false));
        run(&mut c, &mut m, 480);
        c.handle_input(&key("KeyL", false, false));
        run(&mut c, &mut m, 480);
        let expect = if i % 2 == 0 {
            CartState::Playing
        } else {
            CartState::Stopping
        };
        assert_eq!(state(&c, cart), expect, "press {}", i + 1);
    }
    run(&mut c, &mut m, 9600);
    assert_eq!(
        state(&c, cart),
        CartState::Ready,
        "even number of presses ends stopped"
    );
}

#[test]
fn tile_time_reflects_trim_and_speed() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap(); // 20 s
    wait_ready(&mut c);
    c.edit_cart(cart, |x| {
        x.trim = Some(Trim {
            in_frames: 48_000,
            out_frames: 48_000 * 5,
        }); // 4 s
        x.speed = 2.0;
    });
    let rt = c.runtime()[&cart];
    assert!(
        (rt.duration_s - 2.0).abs() < 1e-3,
        "4 s trimmed at 2× = 2 s, got {}",
        rt.duration_s
    );
    c.trigger(cart, TriggerSource::Ui, None);
    run(&mut c, &mut m, 4800); // 0.1 s wall clock = 0.2 s of audio
    let rt = c.runtime()[&cart];
    assert!((rt.remaining_s - 1.9).abs() < 0.01, "{}", rt.remaining_s);
    // x1: spin to normal speed (not the 2× preset) over the speed-up time.
    c.edit_cart(cart, |x| x.speedup_ms = 200);
    c.speed_to_normal(cart);
    run(&mut c, &mut m, 4800);
    let mid = c.runtime()[&cart].speed;
    assert!(mid > 1.3 && mid < 1.7, "half-way from 2× to 1×: {mid}");
    run(&mut c, &mut m, 9600);
    assert_eq!(c.runtime()[&cart].speed, 1.0);
    let peaks = c.take_cart_peaks();
    let (l, _) = peaks[&cart];
    assert!((20.0 * l.log10() + 18.0).abs() < 0.3, "meter sees −18 dBFS");
    // x0: slows to 0 % and the cart ends there.
    c.edit_cart(cart, |x| x.slowdown_ms = 100);
    c.slow_to_stop(cart);
    run(&mut c, &mut m, 9600);
    run(&mut c, &mut m, 960);
    assert_ne!(state(&c, cart), CartState::Playing, "halted carts end");
}

#[test]
fn groups_can_be_renamed_added_and_removed() {
    if missing_test_files() {
        return;
    }
    let (mut c, _m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let g2 = c.project().groups[1].id;
    c.edit_cart(cart, |x| x.group = Some(g2));
    let mut groups = c.project().groups.clone();
    groups[1].name = "Beds".into();
    c.set_groups(groups.clone());
    assert_eq!(c.project().groups[1].label(1), "G2 Beds");
    let id = c.add_group();
    assert_eq!(c.project().groups.len(), 9);
    assert_eq!(c.project().groups[8].id, id);
    // Removing the group a cart uses leaves the cart ungrouped.
    let mut groups = c.project().groups.clone();
    groups.remove(1);
    c.set_groups(groups);
    assert_eq!(c.project().cart(cart).unwrap().group, None);
    assert!(c.undo());
    assert_eq!(c.project().cart(cart).unwrap().group, Some(g2));
}

#[test]
fn x0_halt_ends_the_cart_and_the_next_play_starts_from_in() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let cart = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    c.edit_cart(cart, |x| {
        x.looping = true;
        x.slowdown_ms = 50;
    });
    wait_ready(&mut c);
    let src = BindingSource::Key {
        key: "KeyA".into(),
        modifiers: vec![],
    };
    c.set_binding(cart, src, Action::Toggle);

    // While slowing, a press snaps back to real time and carries on.
    c.pointer_down(cart, MouseButton::Primary);
    run(&mut c, &mut m, 9600);
    c.edit_cart(cart, |x| x.slowdown_ms = 1000);
    c.slow_to_stop(cart);
    run(&mut c, &mut m, 4800);
    assert!(c.runtime()[&cart].slowed);
    c.pointer_down(cart, MouseButton::Primary);
    run(&mut c, &mut m, 960);
    let rt = c.runtime()[&cart];
    assert_eq!(rt.state, CartState::Playing);
    assert_eq!(rt.speed, 1.0);
    assert!(!rt.slowed);

    // Once at 0 % the cart ends; touch plays it again from the In point.
    c.edit_cart(cart, |x| x.slowdown_ms = 50);
    c.slow_to_stop(cart);
    run(&mut c, &mut m, 4800);
    run(&mut c, &mut m, 960);
    assert_ne!(state(&c, cart), CartState::Playing);
    c.pointer_down(cart, MouseButton::Primary);
    run(&mut c, &mut m, 960);
    let rt = c.runtime()[&cart];
    assert_eq!(rt.state, CartState::Playing);
    assert_eq!(rt.speed, 1.0);
    assert!(rt.elapsed_s < 0.05, "restarted from In: {}", rt.elapsed_s);

    // Same with the hotkey.
    c.slow_to_stop(cart);
    run(&mut c, &mut m, 4800);
    run(&mut c, &mut m, 960);
    assert_ne!(state(&c, cart), CartState::Playing);
    c.handle_input(&key("KeyA", true, false));
    c.handle_input(&key("KeyA", false, false));
    run(&mut c, &mut m, 960);
    let rt = c.runtime()[&cart];
    assert_eq!(rt.state, CartState::Playing);
    assert!(rt.elapsed_s < 0.05);
}

#[test]
fn outputs_route_carts_to_devices_and_groups_span_devices() {
    if missing_test_files() {
        return;
    }
    let (ha, mut ma) = Engine::new(EngineConfig::new(SR, 2));
    let (hb, mut mb) = Engine::new(EngineConfig::new(SR, 4));
    let mut c = Controller::new(Controller::new_project("Test"), None);
    c.attach_outputs(
        vec![ha, hb],
        vec![
            Route {
                engine: 0,
                offset: 0,
                channels: 2,
            },
            Route {
                engine: 1,
                offset: 2,
                channels: 2,
            },
        ],
    );
    let a = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let b = c
        .assign_file(0, 0, 1, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let g = c.project().groups[0].id;
    c.edit_cart(a, |x| x.group = Some(g));
    c.edit_cart(b, |x| {
        x.group = Some(g);
        x.output = "B".into();
    });
    wait_ready(&mut c);
    let render = |m: &mut Mixer, ch: usize| {
        let mut out = vec![0.0; 960 * ch];
        m.process(&mut out);
        out
    };

    // Output B: second device, channels 3–4 only.
    c.trigger(b, TriggerSource::Ui, None);
    let ob = render(&mut mb, 4);
    let oa = render(&mut ma, 2);
    c.tick();
    let peak =
        |o: &[f32], ch: usize, k: usize| o.chunks(ch).map(|f| f[k].abs()).fold(0.0f32, f32::max);
    assert!(peak(&ob, 4, 2) > 0.05 && peak(&ob, 4, 3) > 0.05);
    assert_eq!(peak(&ob, 4, 0), 0.0);
    assert_eq!(peak(&oa, 2, 0), 0.0, "nothing on device A");
    assert_eq!(state(&c, b), CartState::Playing);

    // Same exclusive group on Output A stops the cart on the other device.
    c.trigger(a, TriggerSource::Ui, None);
    for _ in 0..20 {
        render(&mut ma, 2);
        render(&mut mb, 4);
    }
    c.tick();
    assert_eq!(state(&c, a), CartState::Playing);
    assert_ne!(
        state(&c, b),
        CartState::Playing,
        "group stop crossed devices"
    );

    // An output not set up on this machine falls back to Output A.
    c.edit_cart(b, |x| x.output = "F".into());
    c.trigger(b, TriggerSource::Ui, None);
    render(&mut ma, 2);
    render(&mut mb, 4);
    c.tick();
    assert_eq!(state(&c, b), CartState::Playing);
}

#[test]
fn stop_group_control_button_stops_its_group_only() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let a = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let b = c
        .assign_file(0, 0, 1, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let (g1, g2) = (c.project().groups[0].id, c.project().groups[1].id);
    c.edit_cart(a, |x| x.group = Some(g1));
    c.edit_cart(b, |x| x.group = Some(g2));
    wait_ready(&mut c);
    let stop = c
        .create_control(0, 1, 0, bubble_project::Control::StopGroup, Some(g1))
        .unwrap();
    let cart = c.project().cart(stop).unwrap();
    assert_eq!(cart.label, "Stop");
    assert!(cart.asset.is_none());
    assert_eq!(
        state(&c, stop),
        CartState::Ready,
        "control buttons are always ready"
    );

    c.trigger(a, TriggerSource::Ui, None);
    c.trigger(b, TriggerSource::Ui, None);
    run(&mut c, &mut m, 960);
    c.pointer_down(stop, MouseButton::Primary);
    run(&mut c, &mut m, 48_000);
    assert_ne!(state(&c, a), CartState::Playing, "group 1 stopped");
    assert_eq!(state(&c, b), CartState::Playing, "group 2 untouched");
}

#[test]
fn pages_move_and_paste_replaces_layout() {
    if missing_test_files() {
        return;
    }
    let (mut c, _m) = setup();
    c.assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    let p2 = c.add_page("Two", 2, 3);
    c.set_current_page(0);
    c.move_page(0, p2);
    assert_eq!(c.project().pages[0].name, "Two");
    assert_eq!(c.current_page(), 1, "current page follows the move");
    let clip = c.copy_page(1).unwrap();
    c.paste_page(0, &clip);
    let p = &c.project().pages[0];
    assert_eq!((p.rows, p.cols, p.cells.len()), (4, 6, 1));
    assert!(c.undo());
    assert_eq!(c.project().pages[0].cells.len(), 0);
}

#[test]
fn preview_plays_unsaved_trim() {
    if missing_test_files() {
        return;
    }
    let (mut c, mut m) = setup();
    let id = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_48_AB_M18.wav"))
        .unwrap();
    wait_ready(&mut c);
    let mut draft = c.project().cart(id).unwrap().clone();
    draft.trim = Some(Trim {
        in_frames: 0,
        out_frames: 4_800,
    });
    assert!(c.preview(&draft));
    run(&mut c, &mut m, 960);
    let rt = c.runtime()[&id];
    assert!((rt.duration_s - 0.1).abs() < 1e-3, "{}", rt.duration_s);
    assert!(
        c.project().cart(id).unwrap().trim.is_none(),
        "project unchanged"
    );
}

#[test]
fn reassign_outputs_by_scope() {
    if missing_test_files() {
        return;
    }
    let (mut c, _m) = setup();
    let f = test_file("1kHz_16_48_AB_M18.wav");
    let a = c.assign_file(0, 0, 0, &f).unwrap();
    let b = c.assign_file(0, 0, 1, &f).unwrap();
    let p2 = c.add_page("Two", 2, 2);
    let d = c.assign_file(p2, 0, 0, &f).unwrap();
    for id in [a, b, d] {
        c.edit_cart(id, |x| x.output = "B".into());
    }
    // Buttons 2–2 on page 1 only.
    let n = c.reassign_outputs(
        Some("B"),
        "A",
        &ReassignScope::Buttons {
            page: 0,
            first: 2,
            last: 2,
        },
    );
    assert_eq!(n, 1);
    assert_eq!(c.project().cart(b).unwrap().output, "A");
    assert_eq!(c.project().cart(a).unwrap().output, "B");
    // Everything from any output.
    assert_eq!(c.reassign_outputs(None, "A", &ReassignScope::All), 2);
    assert!(c.undo());
    assert_eq!(c.project().cart(d).unwrap().output, "B");
}

#[test]
fn asset_cache_refuses_audio_beyond_its_budget() {
    if missing_test_files() {
        return;
    }
    let (mut c, _m) = setup();
    c.assign_file(0, 0, 0, &test_file("ding_16_48_AB.wav"))
        .unwrap();
    let project = c.project().clone();
    let settle = |cache: &mut AssetCache| {
        let t = Instant::now();
        while cache.pending() > 0 {
            cache.poll();
            assert!(t.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    let mut tight = AssetCache::with_budget(1_000);
    tight.rearm_all(&project, None, SR);
    settle(&mut tight);
    let id = project.assets[0].id;
    match tight.state(id) {
        Some(AssetState::Error { message, .. }) => {
            assert!(message.contains("too long to hold in memory"), "{message}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(tight.memory().0, 0);

    let mut roomy = AssetCache::with_budget(1_000_000_000);
    roomy.rearm_all(&project, None, SR);
    settle(&mut roomy);
    assert!(roomy.state(id).and_then(AssetState::buffer).is_some());
    assert!(roomy.memory().0 > 0, "prepared audio is counted");
}

#[test]
fn converting_to_wav_keeps_the_trimmed_duration() {
    if missing_test_files() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (mut c, mut m) = setup();
    let a = c
        .assign_file(0, 0, 0, &test_file("1kHz_16_44_AB_0.wav"))
        .unwrap();
    wait_ready(&mut c);
    c.edit_cart(a, |x| {
        x.trim = Some(Trim {
            in_frames: 4410,
            out_frames: 8820,
        }); // 100–200 ms
    });
    let asset = c.project().cart(a).unwrap().asset.unwrap();
    let src = c.asset_files()[0].1.clone();
    let wav = dir.path().join("tone.wav");
    let info = bubble_engine::loader::convert_to_wav(&src, &wav, SR).unwrap();
    c.apply_conversions(&[Conversion {
        asset,
        file: wav.clone(),
        from_rate: info.sample_rate,
        to_rate: SR,
    }]);
    wait_ready(&mut c);
    let t = c.project().cart(a).unwrap().trim.unwrap();
    assert_eq!(
        (t.in_frames, t.out_frames),
        (4800, 9600),
        "rescaled to 48 kHz"
    );
    assert_eq!(c.asset_files()[0].1, wav, "asset points at the WAV");
    assert!(c.is_dirty());
    c.trigger(a, TriggerSource::Ui, None);
    run(&mut c, &mut m, 64);
    let rt = c.runtime()[&a];
    assert!((rt.duration_s - 0.1).abs() < 0.001, "{}", rt.duration_s);
    assert_eq!(state(&c, a), CartState::Playing);
}
