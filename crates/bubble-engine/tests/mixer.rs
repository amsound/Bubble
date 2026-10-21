//! Offline tests of the real-time mixer: command handling, voice management and
//! audible output, rendered without an audio device.

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bubble_engine::{
    CartKey, EndReason, Engine, EngineCommand, EngineConfig, EngineEvent, EngineHandle, Fade,
    FadeCurve, Mixer, PlayParams, Playable, Retrigger, SampleBuffer, StopOthers, StopTarget,
};

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

const SR: u32 = 48_000;

/// Engine for exact-sample tests: no start de-click ramp.
fn engine(voices: usize) -> (EngineHandle, Mixer) {
    let mut cfg = EngineConfig::new(SR, 2);
    cfg.voices = voices;
    cfg.declick_ms = 0.0;
    Engine::new(cfg)
}

/// Constant stereo buffer (L = value, R = value).
fn dc(value: f32, frames: usize) -> Arc<Playable> {
    Playable::resident(SampleBuffer::from_interleaved(
        SR,
        2,
        vec![value; frames * 2],
    ))
}

/// Stereo ramp: sample n = n (both channels) — lets tests read the playback position.
fn ramp(frames: usize) -> Arc<Playable> {
    let mut v = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        v.push(n as f32);
        v.push(n as f32);
    }
    Playable::resident(SampleBuffer::from_interleaved(SR, 2, v))
}

fn render(m: &mut Mixer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * 2];
    assert_no_alloc(|| m.process(&mut out));
    out
}

fn events(h: &mut EngineHandle) -> Vec<EngineEvent> {
    let mut v = Vec::new();
    h.poll_events(|e| v.push(e));
    v
}

fn play(h: &mut EngineHandle, cart: u32, buf: &Arc<Playable>, p: PlayParams) {
    h.send(EngineCommand::Play {
        cart: CartKey(cart),
        audio: buf.clone(),
        stream: None,
        params: p,
    })
    .unwrap();
}

fn no_limiter(h: &mut EngineHandle) {
    h.send(EngineCommand::SetLimiter { enabled: false })
        .unwrap();
}

#[test]
fn play_is_sample_exact_from_first_frame() {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let buf = ramp(1000);
    play(&mut h, 1, &buf, PlayParams::default());
    let out = render(&mut m, 100);
    for n in 0..100 {
        assert_eq!(out[n * 2], n as f32, "left frame {n}");
        assert_eq!(out[n * 2 + 1], n as f32, "right frame {n}");
    }
    assert!(matches!(
        events(&mut h)[0],
        EngineEvent::VoiceStarted {
            cart: CartKey(1),
            ..
        }
    ));
}

#[test]
fn gain_is_applied_and_independent_per_cart() {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let a = dc(0.25, 10_000);
    let b = dc(0.125, 10_000);
    play(
        &mut h,
        1,
        &a,
        PlayParams {
            gain: 0.5,
            ..Default::default()
        },
    );
    play(
        &mut h,
        2,
        &b,
        PlayParams {
            gain: 2.0,
            ..Default::default()
        },
    );
    let out = render(&mut m, 64);
    // 0.25*0.5 + 0.125*2.0 = 0.375
    assert!((out[10] - 0.375).abs() < 1e-6, "{}", out[10]);
    // Change cart 1 gain to 0: after the 10 ms ramp only cart 2 remains.
    h.send(EngineCommand::SetGain {
        cart: CartKey(1),
        gain: 0.0,
    })
    .unwrap();
    let out = render(&mut m, 1000);
    assert!((out[999 * 2] - 0.25).abs() < 1e-6, "{}", out[999 * 2]);
}

#[test]
fn simultaneous_voices_sum() {
    let (mut h, mut m) = engine(64);
    no_limiter(&mut h);
    let buf = dc(0.01, 4800);
    for c in 0..40 {
        play(&mut h, c, &buf, PlayParams::default());
    }
    let out = render(&mut m, 256);
    assert!((out[100] - 0.40).abs() < 1e-5, "{}", out[100]);
    assert_eq!(h.status().active_voices(), 40);
}

#[test]
fn completion_emits_event_and_frees_voice() {
    let (mut h, mut m) = engine(4);
    let buf = dc(0.1, 300);
    play(&mut h, 7, &buf, PlayParams::default());
    let out = render(&mut m, 512);
    assert!(out[299 * 2] > 0.0);
    assert_eq!(out[300 * 2], 0.0, "silent after end");
    let ev = events(&mut h);
    assert!(ev.iter().any(|e| matches!(
        e,
        EngineEvent::VoiceEnded {
            cart: CartKey(7),
            reason: EndReason::Completed,
            ..
        }
    )));
    assert_eq!(h.status().active_voices(), 0);
    // Buffer returned through the garbage ring: only our reference remains.
    assert_eq!(Arc::strong_count(&buf), 1);
}

#[test]
fn looping_wraps_between_in_and_out_points() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = ramp(100);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            looping: true,
            start_frame: 10,
            end_frame: Some(20),
            ..Default::default()
        },
    );
    let out = render(&mut m, 35);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let expected: Vec<f32> = (0..35).map(|i| (10 + i % 10) as f32).collect();
    assert_eq!(left, expected);
    // Turning loop off lets it finish at the Out point.
    h.send(EngineCommand::SetLoop {
        cart: CartKey(1),
        looping: false,
    })
    .unwrap();
    render(&mut m, 64);
    assert_eq!(h.status().active_voices(), 0);
}

#[test]
fn stop_with_fade_reaches_silence_after_fade_length() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = dc(0.5, SR as usize);
    play(&mut h, 1, &buf, PlayParams::default());
    render(&mut m, 128);
    let fade = Fade {
        frames: 480,
        curve: FadeCurve::Linear,
    };
    h.send(EngineCommand::Stop {
        target: StopTarget::Cart(CartKey(1)),
        fade: Some(fade),
    })
    .unwrap();
    let out = render(&mut m, 1000);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    // Linear fade: halfway point ≈ half level, monotonic decrease, silent after 480 frames.
    assert!((left[239] - 0.25).abs() < 0.01, "{}", left[239]);
    assert!(left.windows(2).take(479).all(|w| w[1] <= w[0]));
    assert!(left[480..].iter().all(|s| *s == 0.0));
    let ev = events(&mut h);
    assert!(ev.iter().any(|e| matches!(
        e,
        EngineEvent::VoiceEnded {
            reason: EndReason::Stopped,
            ..
        }
    )));
}

#[test]
fn stop_without_fade_uses_short_anti_click_ramp() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = dc(0.5, SR as usize);
    play(&mut h, 1, &buf, PlayParams::default());
    render(&mut m, 64);
    h.send(EngineCommand::Kill {
        target: StopTarget::All,
    })
    .unwrap();
    let out = render(&mut m, 200);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert!(left[0] > 0.4, "ramp starts near full level");
    assert!(
        left[96..].iter().all(|s| *s == 0.0),
        "2 ms at 48 kHz = 96 frames"
    );
}

#[test]
fn fade_in_rises_from_silence() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = dc(1.0, SR as usize);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            fade_in: Fade {
                frames: 100,
                curve: FadeCurve::Linear,
            },
            ..Default::default()
        },
    );
    let out = render(&mut m, 200);
    assert!((out[0] - 0.01).abs() < 1e-6);
    assert!((out[49 * 2] - 0.5).abs() < 1e-6);
    assert_eq!(out[150 * 2], 1.0);
}

#[test]
fn retrigger_restart_keeps_single_voice_and_restarts() {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let buf = ramp(10_000);
    play(&mut h, 1, &buf, PlayParams::default());
    render(&mut m, 500);
    play(&mut h, 1, &buf, PlayParams::default());
    render(&mut m, 200); // old voice fades out over 96 frames
    let ev = events(&mut h);
    assert!(ev.iter().any(|e| matches!(
        e,
        EngineEvent::VoiceEnded {
            reason: EndReason::Restarted,
            ..
        }
    )));
    assert_eq!(h.status().active_voices(), 1);
    let mut pos = 0;
    h.status().for_each_voice(|v| pos = v.position_frames);
    assert_eq!(pos, 200, "new voice started from the In point");
}

#[test]
fn retrigger_overlap_adds_voices() {
    let (mut h, mut m) = engine(8);
    let buf = dc(0.1, 10_000);
    for _ in 0..3 {
        play(
            &mut h,
            1,
            &buf,
            PlayParams {
                retrigger: Retrigger::Overlap,
                ..Default::default()
            },
        );
        render(&mut m, 100);
    }
    assert_eq!(h.status().active_voices(), 3);
}

#[test]
fn retrigger_toggle_stops_then_plays() {
    let (mut h, mut m) = engine(8);
    let buf = dc(0.1, 100_000);
    let p = PlayParams {
        retrigger: Retrigger::ToggleStop,
        ..Default::default()
    };
    play(&mut h, 1, &buf, p);
    render(&mut m, 100);
    play(&mut h, 1, &buf, p);
    render(&mut m, 200);
    assert_eq!(h.status().active_voices(), 0, "second trigger stopped it");
    play(&mut h, 1, &buf, p);
    render(&mut m, 100);
    assert_eq!(h.status().active_voices(), 1, "third trigger plays again");
}

#[test]
fn retrigger_ignore_while_playing() {
    let (mut h, mut m) = engine(8);
    let buf = dc(0.1, 100_000);
    let p = PlayParams {
        retrigger: Retrigger::Ignore,
        ..Default::default()
    };
    play(&mut h, 1, &buf, p);
    render(&mut m, 100);
    play(&mut h, 1, &buf, p);
    render(&mut m, 100);
    assert_eq!(h.status().active_voices(), 1);
    assert!(
        events(&mut h)
            .iter()
            .any(|e| matches!(e, EngineEvent::PlayIgnored { .. }))
    );
}

#[test]
fn exclusive_group_stops_other_members() {
    let (mut h, mut m) = engine(8);
    let buf = dc(0.1, 100_000);
    let fade = Fade {
        frames: 100,
        curve: FadeCurve::Linear,
    };
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            group: 3,
            fade_out: fade,
            ..Default::default()
        },
    );
    play(&mut h, 2, &buf, PlayParams::default());
    render(&mut m, 100);
    play(
        &mut h,
        5,
        &buf,
        PlayParams {
            group: 3,
            ..Default::default()
        },
    );
    render(&mut m, 200);
    let mut carts = Vec::new();
    h.status().for_each_voice(|v| carts.push(v.cart.0));
    carts.sort();
    assert_eq!(
        carts,
        vec![2, 5],
        "cart 1 (same group) stopped, cart 2 untouched"
    );
}

#[test]
fn stop_others_page_and_all() {
    let (mut h, mut m) = engine(8);
    let buf = dc(0.1, 100_000);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            page: 1,
            ..Default::default()
        },
    );
    play(
        &mut h,
        2,
        &buf,
        PlayParams {
            page: 2,
            ..Default::default()
        },
    );
    render(&mut m, 50);
    play(
        &mut h,
        3,
        &buf,
        PlayParams {
            page: 1,
            stop_others: StopOthers::Page,
            ..Default::default()
        },
    );
    render(&mut m, 200);
    let mut carts = Vec::new();
    h.status().for_each_voice(|v| carts.push(v.cart.0));
    carts.sort();
    assert_eq!(carts, vec![2, 3]);

    h.send(EngineCommand::Stop {
        target: StopTarget::Page(2),
        fade: None,
    })
    .unwrap();
    render(&mut m, 200);
    let mut carts = Vec::new();
    h.status().for_each_voice(|v| carts.push(v.cart.0));
    assert_eq!(carts, vec![3]);

    play(
        &mut h,
        4,
        &buf,
        PlayParams {
            stop_others: StopOthers::All,
            ..Default::default()
        },
    );
    render(&mut m, 200);
    let mut carts = Vec::new();
    h.status().for_each_voice(|v| carts.push(v.cart.0));
    assert_eq!(carts, vec![4]);
}

#[test]
fn full_pool_steals_oldest_voice() {
    let (mut h, mut m) = engine(4);
    let buf = dc(0.01, 100_000);
    for c in 0..5 {
        play(&mut h, c, &buf, PlayParams::default());
        render(&mut m, 10);
    }
    assert_eq!(h.status().active_voices(), 4);
    let ev = events(&mut h);
    assert!(ev.iter().any(|e| matches!(
        e,
        EngineEvent::VoiceEnded {
            cart: CartKey(0),
            reason: EndReason::Stolen,
            ..
        }
    )));
    assert_eq!(h.status().diagnostics().voices_stolen, 1);
}

#[test]
fn output_is_independent_of_callback_size() {
    // The same command timeline rendered with different (non-power-of-two) callback
    // sizes must produce identical audio.
    let reference = render_with_blocks(&[4096]);
    for blocks in [
        &[1usize][..],
        &[37],
        &[480],
        &[511],
        &[1000],
        &[7, 333, 64, 1, 999],
    ] {
        let out = render_with_blocks(blocks);
        assert_eq!(out.len(), reference.len());
        let max_diff = out
            .iter()
            .zip(&reference)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 1e-6, "blocks {blocks:?} differ by {max_diff}");
    }
}

fn render_with_blocks(pattern: &[usize]) -> Vec<f32> {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let buf = ramp(3000);
    // All commands are queued up front, so they apply in the first block regardless of size.
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            fade_in: Fade {
                frames: 300,
                curve: FadeCurve::EqualPower,
            },
            looping: true,
            start_frame: 100,
            end_frame: Some(2100),
            gain: 0.001,
            ..Default::default()
        },
    );
    play(
        &mut h,
        2,
        &buf,
        PlayParams {
            gain: 0.0005,
            pan: 0.3,
            ..Default::default()
        },
    );
    let total = 4096;
    let mut out = vec![0.0; total * 2];
    let mut pos = 0;
    let mut i = 0;
    while pos < total {
        let n = pattern[i % pattern.len()].min(total - pos);
        assert_no_alloc(|| m.process(&mut out[pos * 2..(pos + n) * 2]));
        pos += n;
        i += 1;
    }
    out
}

#[test]
fn limiter_keeps_output_below_ceiling() {
    let (mut h, mut m) = engine(16);
    let buf = dc(0.5, 10_000);
    for c in 0..8 {
        play(&mut h, c, &buf, PlayParams::default());
    }
    let out = render(&mut m, 2048);
    let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    assert!(peak <= 0.967, "peak {peak}");
    assert!(h.status().diagnostics().limiter_active_blocks > 0);
}

#[test]
fn empty_or_invalid_range_is_ignored_without_leaking() {
    let (mut h, mut m) = engine(4);
    let buf = dc(0.1, 100);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            start_frame: 500,
            end_frame: Some(50),
            ..Default::default()
        },
    );
    render(&mut m, 10);
    // start ≥ end is clamped to start = 0, so it plays.
    assert_eq!(h.status().active_voices(), 1);
    let empty = Playable::resident(SampleBuffer::from_interleaved(SR, 2, vec![]));
    play(&mut h, 2, &empty, PlayParams::default());
    render(&mut m, 10);
    assert!(
        events(&mut h)
            .iter()
            .any(|e| matches!(e, EngineEvent::PlayIgnored { cart: CartKey(2) }))
    );
    assert_eq!(
        Arc::strong_count(&empty),
        1,
        "rejected buffer returned via garbage ring"
    );
}

#[test]
fn mono_source_pans_with_constant_power() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let mono = Playable::resident(SampleBuffer::from_interleaved(SR, 1, vec![1.0; 1000]));
    play(&mut h, 1, &mono, PlayParams::default());
    let out = render(&mut m, 10);
    assert!((out[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!((out[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    h.send(EngineCommand::Kill {
        target: StopTarget::All,
    })
    .unwrap();
    render(&mut m, 200);
    play(
        &mut h,
        1,
        &mono,
        PlayParams {
            pan: -1.0,
            ..Default::default()
        },
    );
    let out = render(&mut m, 10);
    assert!((out[0] - 1.0).abs() < 1e-6 && out[1].abs() < 1e-6);
}

#[test]
fn master_gain_applies_to_mix() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = dc(0.5, 10_000);
    h.send(EngineCommand::SetMaster { gain: 0.5 }).unwrap();
    play(&mut h, 1, &buf, PlayParams::default());
    let out = render(&mut m, 1000);
    assert!((out[999 * 2] - 0.25).abs() < 1e-6);
}

#[test]
fn speed_changes_read_rate_with_interpolation() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = ramp(10_000);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            speed: 2.0,
            ..Default::default()
        },
    );
    let out = render(&mut m, 10);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert_eq!(
        left,
        (0..10).map(|n| (n * 2) as f32).collect::<Vec<_>>(),
        "2× skips frames"
    );
    h.send(EngineCommand::Kill {
        target: StopTarget::All,
    })
    .unwrap();
    render(&mut m, 200);
    play(
        &mut h,
        2,
        &buf,
        PlayParams {
            speed: 0.5,
            ..Default::default()
        },
    );
    let out = render(&mut m, 6);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert_eq!(left, vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5], "½× interpolates");
}

#[test]
fn slow_to_stop_holds_position_then_spins_up() {
    let (mut h, mut m) = engine(4);
    let buf = dc(0.1, SR as usize * 10);
    play(&mut h, 1, &buf, PlayParams::default());
    render(&mut m, 4800);
    // Record-player stop over 0.5 s, then wait.
    h.send(EngineCommand::SetSpeed {
        cart: CartKey(1),
        speed: 0.0,
        ramp_frames: 24_000,
    })
    .unwrap();
    render(&mut m, 30_000);
    let pos = |h: &EngineHandle| {
        let mut p = 0;
        h.status().for_each_voice(|v| p = v.position_frames);
        p
    };
    let stopped_at = pos(&h);
    // Linear ramp 1→0 over 24 000 frames covers ~12 000 frames.
    assert!(
        (4800 + 11_900..=4800 + 12_100).contains(&stopped_at),
        "{stopped_at}"
    );
    render(&mut m, 10_000);
    assert_eq!(pos(&h), stopped_at, "holds position at speed 0");
    assert_eq!(
        h.status().active_voices(),
        1,
        "voice stays alive while stopped"
    );
    h.send(EngineCommand::SetSpeed {
        cart: CartKey(1),
        speed: 1.0,
        ramp_frames: 480,
    })
    .unwrap();
    render(&mut m, 4800);
    assert!(pos(&h) > stopped_at + 4000);
    let mut speed = 0.0;
    h.status().for_each_voice(|v| speed = v.speed);
    assert_eq!(speed, 1.0);
}

#[test]
fn looping_at_high_speed_wraps_correctly() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let buf = ramp(100);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            speed: 3.0,
            looping: true,
            start_frame: 10,
            end_frame: Some(15),
            ..Default::default()
        },
    );
    let out = render(&mut m, 6);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    // 10, 13, (16→11), 14, (17→12), (15→10)
    assert_eq!(left, vec![10.0, 13.0, 11.0, 14.0, 12.0, 10.0]);
}

#[test]
fn per_voice_peaks_are_reported_and_reset() {
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let a = dc(0.5, 10_000);
    let b = dc(0.25, 10_000);
    play(&mut h, 1, &a, PlayParams::default());
    play(
        &mut h,
        2,
        &b,
        PlayParams {
            gain: 0.5,
            ..Default::default()
        },
    );
    render(&mut m, 256);
    let mut peaks = std::collections::HashMap::new();
    h.status().take_voice_peaks(|c, l, r| {
        peaks.insert(c.0, (l, r));
    });
    assert_eq!(peaks[&1], (0.5, 0.5));
    assert_eq!(peaks[&2], (0.125, 0.125));
    let mut again = 0.0;
    h.status()
        .take_voice_peaks(|_, l, _| again = f32::max(again, l));
    assert_eq!(again, 0.0, "taken peaks reset until the next block");
}

fn engine_ch(channels: u16) -> (EngineHandle, Mixer) {
    let mut cfg = EngineConfig::new(SR, channels);
    cfg.declick_ms = 0.0;
    Engine::new(cfg)
}

fn render_ch(m: &mut Mixer, frames: usize, channels: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * channels];
    assert_no_alloc(|| m.process(&mut out));
    out
}

#[test]
fn outputs_route_to_their_channels_on_one_device() {
    let (mut h, mut m) = engine_ch(8);
    no_limiter(&mut h);
    let a = Playable::resident(SampleBuffer::from_interleaved(
        SR,
        2,
        [0.25f32, -0.25].repeat(100),
    ));
    // Stereo output on channels 3–4 (offset 2).
    play(
        &mut h,
        1,
        &a,
        PlayParams {
            output_offset: 2,
            ..Default::default()
        },
    );
    // Mono output on channel 8: L+R summed at half each.
    let b = Playable::resident(SampleBuffer::from_interleaved(
        SR,
        2,
        [0.4f32, 0.2].repeat(100),
    ));
    play(
        &mut h,
        2,
        &b,
        PlayParams {
            output_offset: 7,
            output_channels: 1,
            ..Default::default()
        },
    );
    let out = render_ch(&mut m, 10, 8);
    for f in 0..10 {
        let fr = &out[f * 8..f * 8 + 8];
        assert_eq!(fr[2], 0.25);
        assert_eq!(fr[3], -0.25);
        assert!((fr[7] - 0.3).abs() < 1e-6);
        for c in [0, 1, 4, 5, 6] {
            assert_eq!(fr[c], 0.0, "channel {c} silent");
        }
    }
}

#[test]
fn surround_output_passes_six_channels_through() {
    let (mut h, mut m) = engine_ch(8);
    no_limiter(&mut h);
    let frame: Vec<f32> = (1..=6).map(|c| c as f32 * 0.1).collect();
    let s = Playable::resident(SampleBuffer::from_interleaved(SR, 6, frame.repeat(50)));
    play(
        &mut h,
        1,
        &s,
        PlayParams {
            output_offset: 2,
            output_channels: 6,
            ..Default::default()
        },
    );
    let out = render_ch(&mut m, 4, 8);
    for f in 0..4 {
        let fr = &out[f * 8..f * 8 + 8];
        assert_eq!(&fr[..2], &[0.0, 0.0]);
        for c in 0..6 {
            assert!((fr[2 + c] - frame[c]).abs() < 1e-6, "ch {c}: {}", fr[2 + c]);
        }
    }
}

#[test]
fn route_outside_the_device_falls_back_to_its_first_channels() {
    let (mut h, mut m) = engine_ch(2);
    no_limiter(&mut h);
    let a = dc(0.5, 100);
    play(
        &mut h,
        1,
        &a,
        PlayParams {
            output_offset: 6,
            ..Default::default()
        },
    );
    let out = render_ch(&mut m, 4, 2);
    assert_eq!(&out[..2], &[0.5, 0.5]);
}

#[test]
fn group_stop_target_stops_only_that_group() {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let a = dc(0.1, 48_000);
    play(
        &mut h,
        1,
        &a,
        PlayParams {
            group: 3,
            ..Default::default()
        },
    );
    play(
        &mut h,
        2,
        &a,
        PlayParams {
            group: 4,
            ..Default::default()
        },
    );
    render(&mut m, 64);
    h.send(EngineCommand::Stop {
        target: StopTarget::Group(3),
        fade: Some(Fade::NONE),
    })
    .unwrap();
    render(&mut m, 512);
    let ended: Vec<u32> = events(&mut h)
        .into_iter()
        .filter_map(|e| match e {
            EngineEvent::VoiceEnded { cart, .. } => Some(cart.0),
            _ => None,
        })
        .collect();
    assert_eq!(ended, [1]);
}

#[test]
fn starting_on_a_loud_sample_ramps_in_and_a_fading_loop_ends() {
    let (mut h, mut m) = Engine::new(EngineConfig::new(SR, 2)); // default 1.5 ms de-click
    no_limiter(&mut h);
    let a = dc(0.5, 4_800);
    play(&mut h, 1, &a, PlayParams::default());
    let out = render(&mut m, 200);
    assert!(out[0].abs() < 0.02, "starts near silence: {}", out[0]);
    assert!((out[150 * 2] - 0.5).abs() < 1e-6, "full level after 1.5 ms");

    // A looping cart told to stop with a fade longer than what is left plays to its
    // Out point and ends instead of looping again.
    let (mut h, mut m) = engine(4);
    no_limiter(&mut h);
    let short = dc(0.25, 960); // 20 ms
    play(
        &mut h,
        2,
        &short,
        PlayParams {
            looping: true,
            fade_out: Fade::from_ms(3_000.0, SR, FadeCurve::Linear),
            ..Default::default()
        },
    );
    render(&mut m, 480);
    h.send(EngineCommand::Stop {
        target: StopTarget::Cart(CartKey(2)),
        fade: None,
    })
    .unwrap();
    render(&mut m, 2_000);
    let ended = events(&mut h).into_iter().any(|e| {
        matches!(
            e,
            EngineEvent::VoiceEnded {
                cart: CartKey(2),
                reason: EndReason::Stopped,
                ..
            }
        )
    });
    assert!(ended, "ended at the Out point while fading");
}

#[test]
fn seek_jumps_with_a_crossfade_and_keeps_pause_and_loop() {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let buf = ramp(20_000);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            start_frame: 1_000,
            end_frame: Some(15_000),
            looping: true,
            ..Default::default()
        },
    );
    render(&mut m, 100);
    h.send(EngineCommand::Seek {
        cart: CartKey(1),
        frame: 14_900,
    })
    .unwrap();
    let out = render(&mut m, 400);
    // Old position fades out over ~2 ms (96 frames) while the new one fades in.
    let mid = out[50 * 2];
    assert!(mid > 1_100.0 && mid < 14_900.0, "crossfading: {mid}");
    // After the ramp only the new position sounds; it reached Out and looped to In.
    let late = out[300 * 2];
    assert!(
        (late - (1_000.0 + 200.0)).abs() < 2.0,
        "looped from Out to In: {late}"
    );
    let ev = events(&mut h);
    assert!(ev.iter().any(|e| matches!(
        e,
        EngineEvent::VoiceEnded {
            reason: EndReason::Restarted,
            ..
        }
    )));

    // A paused voice (speed 0) stays paused at the new position.
    h.send(EngineCommand::SetSpeed {
        cart: CartKey(1),
        speed: 0.0,
        ramp_frames: 1,
    })
    .unwrap();
    render(&mut m, 10);
    h.send(EngineCommand::Seek {
        cart: CartKey(1),
        frame: 5_000,
    })
    .unwrap();
    render(&mut m, 400);
    let mut positions = Vec::new();
    h.status()
        .for_each_voice(|v| positions.push(v.position_frames));
    // Positions are reported from the In point.
    assert!(
        positions.contains(&4_000),
        "paused at the new place: {positions:?}"
    );
    render(&mut m, 400);
    let mut again = Vec::new();
    h.status().for_each_voice(|v| again.push(v.position_frames));
    assert!(again.contains(&4_000), "still paused: {again:?}");
}

#[test]
fn live_region_change_applies_to_the_playing_voice() {
    let (mut h, mut m) = engine(8);
    no_limiter(&mut h);
    let buf = ramp(20_000);
    play(
        &mut h,
        1,
        &buf,
        PlayParams {
            looping: true,
            ..Default::default()
        },
    );
    render(&mut m, 5_000);
    // New Out before the playhead: a looping voice goes to the new In.
    h.send(EngineCommand::SetRegion {
        cart: CartKey(1),
        start: 2_000,
        end: 4_000,
    })
    .unwrap();
    let out = render(&mut m, 2_100);
    assert_eq!(out[0], 2_000.0, "jumped to the new In");
    assert_eq!(out[1_999 * 2], 3_999.0, "plays to the new Out");
    assert_eq!(out[2_000 * 2], 2_000.0, "and loops to the new In");
}
