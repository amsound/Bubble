//! Streaming playback (decision D-37): a voice of a long file starts from its pre-roll
//! head and the disk reader takes over. Each test renders the same cart from a
//! streamed and an in-memory copy of one file and expects the same output, sample
//! for sample, through loops, seeks, speed changes and trims, without allocating on
//! the audio thread; underruns give silence, hold the place and are counted.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use bubble_engine::loader::{PrepareOptions, load_for_engine, prepare};
use bubble_engine::{
    CartKey, Engine, EngineCommand, EngineConfig, EngineHandle, Mixer, PlayParams, Playable,
    Retrigger, SectionPlan, Streamer,
};

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

const SR: u32 = 48_000;
/// Pre-roll used here: 0.1 s, so the disk takes over early and often.
const HEAD: f64 = 0.1;

/// Writes a 32-bit float stereo WAV whose sample n is (n, −n) at `rate`, so output
/// values read back as positions.
fn ramp_wav(path: &Path, frames: usize, rate: u32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for n in 0..frames {
        w.write_sample(n as f32).unwrap();
        w.write_sample(-(n as f32)).unwrap();
    }
    w.finalize().unwrap();
}

/// A 16-bit stereo WAV with a 440 Hz / 660 Hz tone.
fn tone_wav(path: &Path, frames: usize, rate: u32) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for n in 0..frames {
        let t = n as f32 / rate as f32;
        let l = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
        let r = (t * 660.0 * std::f32::consts::TAU).sin() * 0.5;
        w.write_sample((l * 32767.0) as i16).unwrap();
        w.write_sample((r * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
}

fn streamed(path: &Path, starts: &[u64]) -> Arc<Playable> {
    let opts = PrepareOptions {
        stream_min_secs: 0.0,
        head_secs: HEAD,
    };
    let (p, _) = prepare(path, SR, starts, opts, u64::MAX).unwrap();
    assert!(p.is_streamed());
    p
}

fn resident(path: &Path) -> Arc<Playable> {
    let b = load_for_engine(path, SR).unwrap();
    Playable::resident(Arc::try_unwrap(b).unwrap())
}

struct Rig {
    h: EngineHandle,
    m: Mixer,
    streamer: Streamer,
}

impl Rig {
    fn new() -> Self {
        Self::with_voices(8)
    }

    fn with_voices(voices: usize) -> Self {
        let mut cfg = EngineConfig::new(SR, 2);
        cfg.voices = voices;
        cfg.declick_ms = 0.0;
        let (mut h, m) = Engine::new(cfg);
        h.send(EngineCommand::SetLimiter { enabled: false })
            .unwrap();
        Rig {
            h,
            m,
            streamer: Streamer::new(),
        }
    }

    fn play(&mut self, cart: u32, audio: &Arc<Playable>, p: PlayParams) {
        let start = p.start_frame;
        let stream = self.streamer.open(audio, start);
        assert_eq!(stream.is_some(), audio.is_streamed());
        self.h
            .send(EngineCommand::Play {
                cart: CartKey(cart),
                audio: audio.clone(),
                stream,
                params: p,
            })
            .unwrap();
    }

    fn send(&mut self, cmd: EngineCommand) {
        self.h.send(cmd).unwrap();
    }

    /// Renders `frames`, letting the reader catch up before each block (as the real
    /// clock would).
    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        for block in out.chunks_mut(512 * 2) {
            assert!(
                self.streamer.wait_idle(Duration::from_secs(5)),
                "reader stuck"
            );
            assert_no_alloc(|| self.m.process(block));
        }
        self.h.collect_garbage();
        out
    }

    /// Renders `frames` against the clock, `speedup` times faster than real time,
    /// without waiting for the reader: what a device does, so a reader that is late
    /// shows as underruns.
    fn render_paced(&mut self, frames: usize, speedup: f64) -> Vec<f32> {
        let block = 512;
        let per_block = Duration::from_secs_f64(block as f64 / SR as f64 / speedup);
        let start = std::time::Instant::now();
        let mut out = vec![0.0; frames * 2];
        for (i, chunk) in out.chunks_mut(block * 2).enumerate() {
            let due = start + per_block * i as u32;
            if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
            assert_no_alloc(|| self.m.process(chunk));
        }
        self.h.collect_garbage();
        out
    }

    fn underruns(&self) -> (u64, u64) {
        let d = self.h.status().diagnostics();
        (d.stream_underruns, d.stream_underrun_frames)
    }
}

/// Runs `script` against a streamed and an in-memory copy of `path` and checks they
/// sound the same.
fn same_as_resident(
    path: &Path,
    starts: &[u64],
    script: impl Fn(&mut Rig, &Arc<Playable>) -> Vec<f32>,
) -> Vec<f32> {
    let mut a = Rig::new();
    let s = streamed(path, starts);
    let got = script(&mut a, &s);
    let mut b = Rig::new();
    let r = resident(path);
    let want = script(&mut b, &r);
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert!(
            (g - w).abs() <= 1e-4 * w.abs().max(1.0),
            "sample {i} (frame {}): streamed {g}, in memory {w}",
            i / 2
        );
    }
    assert_eq!(a.underruns(), (0, 0), "no underruns");
    got
}

#[test]
fn plays_through_the_head_into_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 2, SR);
    let out = same_as_resident(&path, &[], |r, a| {
        r.play(1, a, PlayParams::default());
        r.render(SR as usize)
    });
    // Past the 0.1 s head, sample for sample.
    for n in [0, 4_799, 4_800, 4_801, 30_000, 47_999] {
        assert_eq!(out[n * 2], n as f32);
        assert_eq!(out[n * 2 + 1], -(n as f32));
    }
}

#[test]
fn loops_from_out_back_to_in_across_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 3, SR);
    // In at 10 000 (a head there), Out at 30 000: each pass leaves the head for the disk.
    let out = same_as_resident(&path, &[10_000], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: 10_000,
                end_frame: Some(30_000),
                looping: true,
                ..Default::default()
            },
        );
        r.render(70_000)
    });
    assert_eq!(out[0], 10_000.0);
    assert_eq!(out[19_999 * 2], 29_999.0);
    assert_eq!(out[20_000 * 2], 10_000.0, "wrapped to In");
    assert_eq!(out[45_000 * 2], 15_000.0, "third pass, from the disk");
}

#[test]
fn a_loop_longer_than_the_head_wraps_without_an_underrun() {
    // The ring is full when the loop wraps (Out is far ahead of In): the reader can
    // only refill from In once the samples it read past Out are dropped, which must
    // happen while the voice still plays In's head, not when the head runs out.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    // Long enough that the reader has filled the whole ring past Out.
    ramp_wav(&path, SR as usize * 20, SR);
    let out = same_as_resident(&path, &[10_000], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: 10_000,
                end_frame: Some(300_000),
                looping: true,
                ..Default::default()
            },
        );
        r.render(310_000)
    });
    assert_eq!(out[290_000 * 2], 10_000.0, "wrapped to In");
    assert_eq!(out[300_000 * 2], 20_000.0, "past In's head, from the disk");
}

#[test]
fn a_loop_stays_sample_exact_pass_after_pass() {
    // A 1 s loop in a long file, 25 times round: each wrap leaves the reader 4 s past
    // Out, to be dropped while In's head plays.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 20, SR);
    let out = same_as_resident(&path, &[100_000], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: 100_000,
                end_frame: Some(148_000),
                looping: true,
                ..Default::default()
            },
        );
        r.render(48_000 * 25)
    });
    for pass in [1usize, 12, 24] {
        assert_eq!(out[(pass * 48_000 + 7) * 2], 100_007.0, "pass {pass}");
    }
}

#[test]
fn a_loop_at_another_speed_wraps_on_the_stream_as_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 20, SR);
    same_as_resident(&path, &[100_000], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: 100_000,
                end_frame: Some(148_000),
                looping: true,
                speed: 1.37,
                ..Default::default()
            },
        );
        r.render(48_000 * 8)
    });
}

#[test]
fn sections_of_a_44_1_khz_file_match_memory_through_many_passes() {
    // Converted to 48 kHz as it is read: the joins and each of Main's passes must land
    // on the same samples as the whole file converted in memory.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone44.wav");
    tone_wav(&path, 44_100 * 20, 44_100);
    let eng = |src: u64| (src as f64 * 48_000.0 / 44_100.0).round() as usize;
    let (intro, main, outro) = ((0u64, 33_333u64), (33_333u64, 77_777u64), (500_000u64, 540_000u64));
    same_as_resident(&path, &[main.0, outro.0], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: eng(intro.0),
                end_frame: Some(eng(intro.1)),
                sections: Some(SectionPlan {
                    intro: true,
                    main: (eng(main.0), eng(main.1)),
                    outro: Some((eng(outro.0), eng(outro.1))),
                }),
                ..Default::default()
            },
        );
        let mut out = r.render(48_000 * 12);
        r.send(EngineCommand::Outro {
            cart: CartKey(1),
            instantly: false,
            region: (eng(outro.0), eng(outro.1)),
        });
        out.extend(r.render(48_000 * 3));
        out
    });
}

#[test]
fn loops_and_sections_keep_up_with_the_clock() {
    // Played against the clock (2 × real time), not waiting for the reader: no gap at
    // any wrap or join. Heads as Bubble keeps them (2 s).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 30, SR);
    let opts = PrepareOptions {
        stream_min_secs: 0.0,
        head_secs: 2.0,
    };
    let starts = [480_000u64, 960_000];
    let (a, _) = prepare(&path, SR, &starts, opts, u64::MAX).unwrap();
    let mut r = Rig::new();
    // A 3 s loop, and a cart in sections (Intro 2 s, Main 3 s, Outro 2 s).
    r.play(
        1,
        &a,
        PlayParams {
            start_frame: 480_000,
            end_frame: Some(624_000),
            looping: true,
            ..Default::default()
        },
    );
    r.play(
        2,
        &a,
        PlayParams {
            start_frame: 0,
            end_frame: Some(96_000),
            sections: Some(SectionPlan {
                intro: true,
                main: (480_000, 624_000),
                outro: Some((960_000, 1_056_000)),
            }),
            ..Default::default()
        },
    );
    r.render_paced(48_000 * 20, 2.0);
    r.send(EngineCommand::Outro {
        cart: CartKey(2),
        instantly: false,
        region: (960_000, 1_056_000),
    });
    r.render_paced(48_000 * 6, 2.0);
    assert_eq!(r.underruns(), (0, 0), "no gaps");
}

#[test]
fn sections_join_across_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 3, SR);
    // Intro 0–20 000, Main 20 000–40 000 (twice), Outro 100 000–110 000: each part
    // leaves its head for the disk, and the Outro starts from a head of its own.
    let out = same_as_resident(&path, &[20_000, 100_000], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: 0,
                end_frame: Some(20_000),
                sections: Some(SectionPlan {
                    intro: true,
                    main: (20_000, 40_000),
                    outro: Some((100_000, 110_000)),
                }),
                ..Default::default()
            },
        );
        let mut out = r.render(50_000);
        r.send(EngineCommand::Outro {
            cart: CartKey(1),
            instantly: false,
            region: (100_000, 110_000),
        });
        out.extend(r.render(25_000));
        out
    });
    assert_eq!(out[19_999 * 2], 19_999.0);
    assert_eq!(out[20_000 * 2], 20_000.0, "into Main");
    assert_eq!(out[40_000 * 2], 20_000.0, "Main again");
    assert_eq!(out[60_000 * 2], 100_000.0, "into the Outro");
    assert_eq!(out[69_999 * 2], 109_999.0);
    assert_eq!(out[70_000 * 2], 0.0, "ended");
}

#[test]
fn seeks_inside_and_outside_the_heads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 3, SR);
    let s = streamed(&path, &[]);
    let mut r = Rig::new();
    r.play(1, &s, PlayParams::default());
    r.render(20_000);
    // Outside any head: the old place fades, the new one waits for the disk.
    r.send(EngineCommand::Seek {
        cart: CartKey(1),
        frame: 100_000,
    });
    let out = r.render(4_096);
    let tail = &out[3_000 * 2..];
    // After the crossfade the new place plays on, frame by frame.
    let first = tail[0];
    assert!(first > 100_000.0 && first < 104_096.0, "{first}");
    for (k, f) in tail.chunks(2).enumerate() {
        assert_eq!(f[0], first + k as f32);
    }
    // Back into the head at 0: instant, and the disk follows on after it.
    r.send(EngineCommand::Seek {
        cart: CartKey(1),
        frame: 1_000,
    });
    let out = r.render(10_000);
    assert_eq!(out[500 * 2], 1_500.0);
    assert_eq!(out[9_999 * 2], 10_999.0);
    assert_eq!(r.underruns(), (0, 0));
    assert_eq!(r.h.status().active_voices(), 1);
}

#[test]
fn seek_matches_memory_playback_when_the_target_is_in_a_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 3, SR);
    same_as_resident(&path, &[60_000], |r, a| {
        r.play(1, a, PlayParams::default());
        r.render(8_000);
        r.send(EngineCommand::Seek {
            cart: CartKey(1),
            frame: 60_000,
        });
        r.render(20_000)
    });
}

#[test]
fn speed_changes_and_pause_read_the_stream_like_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    tone_wav(&path, SR as usize * 4, SR);
    same_as_resident(&path, &[], |r, a| {
        let mut out = Vec::new();
        r.play(
            1,
            a,
            PlayParams {
                speed: 0.75,
                ..Default::default()
            },
        );
        out.extend(r.render(10_000));
        for (speed, ramp) in [(3.7, 2_000), (0.0, 4_000), (1.3, 1_000), (4.0, 0)] {
            r.send(EngineCommand::SetSpeed {
                cart: CartKey(1),
                speed,
                ramp_frames: ramp,
            });
            out.extend(r.render(12_000));
        }
        out
    });
}

#[test]
fn live_trim_and_loop_changes_follow_on_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    tone_wav(&path, SR as usize * 4, SR);
    let s = streamed(&path, &[]);
    let mut r = Rig::new();
    r.play(1, &s, PlayParams::default());
    r.render(30_000);
    // A new In with no head: the voice waits a moment for the disk (not an underrun).
    r.send(EngineCommand::SetRegion {
        cart: CartKey(1),
        start: 40_000,
        end: 60_000,
    });
    r.send(EngineCommand::SetLoop {
        cart: CartKey(1),
        looping: true,
    });
    let out = r.render(50_000);
    let d = r.h.status().diagnostics();
    assert_eq!(d.stream_underruns, 0);
    assert!(d.stream_wait_frames > 0 && d.stream_wait_frames < 2_000);
    assert!(out.iter().any(|x| x.abs() > 0.4), "still playing");
    assert_eq!(r.h.status().active_voices(), 1, "looping");
}

#[test]
fn overlapping_voices_of_one_cart_stream_independently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 2, SR);
    same_as_resident(&path, &[], |r, a| {
        let p = PlayParams {
            retrigger: Retrigger::Overlap,
            ..Default::default()
        };
        r.play(1, a, p);
        let mut out = r.render(9_000);
        r.play(1, a, p);
        out.extend(r.render(20_000));
        out
    });
}

#[test]
fn resampled_streams_match_a_whole_file_resample() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone44.wav");
    tone_wav(&path, 44_100 * 3, 44_100);
    // In at a source frame inside the file, as a trim gives.
    let in_src = 33_333u64;
    let in_eng = (in_src as f64 * 48_000.0 / 44_100.0).round() as usize;
    same_as_resident(&path, &[in_src], |r, a| {
        r.play(
            1,
            a,
            PlayParams {
                start_frame: in_eng,
                end_frame: Some(in_eng + 30_000),
                looping: true,
                ..Default::default()
            },
        );
        let mut out = r.render(40_000);
        r.send(EngineCommand::Seek {
            cart: CartKey(1),
            frame: 2_000,
        });
        out.extend(r.render(10_000));
        out
    });
}

#[test]
fn an_underrun_plays_silence_holds_the_place_and_is_counted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 2, SR);
    let s = streamed(&path, &[]);
    let mut r = Rig::new();
    r.streamer.set_paused(true);
    r.play(1, &s, PlayParams::default());
    // The head (4 800 frames) plays, then nothing comes from the disk.
    let mut out = vec![0.0; 10_000 * 2];
    assert_no_alloc(|| r.m.process(&mut out));
    assert_eq!(out[4_799 * 2], 4_799.0);
    assert!(out[4_800 * 2..].iter().all(|x| *x == 0.0), "silence");
    let (n, frames) = r.underruns();
    assert_eq!((n, frames), (1, 5_200));
    // The disk comes back: playback resumes where it stopped.
    r.streamer.set_paused(false);
    let out = r.render(1_024);
    assert_eq!(out[0], 4_800.0);
    assert_eq!(out[1_023 * 2], 5_823.0);
    assert_eq!(r.underruns(), (1, 5_200));

    // Stopping while starved still ends the voice.
    let mut r = Rig::new();
    r.streamer.set_paused(true);
    r.play(1, &s, PlayParams::default());
    let mut out = vec![0.0; 8_000 * 2];
    assert_no_alloc(|| r.m.process(&mut out));
    assert_eq!(r.underruns().0, 1);
    r.send(EngineCommand::Stop {
        target: bubble_engine::StopTarget::All,
        fade: None,
    });
    assert_no_alloc(|| r.m.process(&mut out[..2_000]));
    assert_eq!(r.h.status().active_voices(), 0);
}

#[test]
fn a_file_shorter_than_stated_ends_the_voice() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 2, SR);
    let s = streamed(&path, &[]);
    // The file is replaced by a shorter one after it was prepared.
    ramp_wav(&path, 20_000, SR);
    let mut r = Rig::new();
    r.play(1, &s, PlayParams::default());
    r.render(30_000);
    assert_eq!(r.h.status().active_voices(), 0);
    let mut ended = false;
    r.h.poll_events(|e| {
        ended |= matches!(
            e,
            bubble_engine::EngineEvent::VoiceEnded {
                reason: bubble_engine::EndReason::Completed,
                ..
            }
        )
    });
    assert!(ended);
}

#[test]
fn streamed_files_hold_only_their_heads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    tone_wav(&path, SR as usize * 40, SR);
    let (p, info) = prepare(&path, SR, &[96_000], PrepareOptions::default(), u64::MAX).unwrap();
    assert!(p.is_streamed());
    assert_eq!(info.frames, SR as u64 * 40);
    assert_eq!(p.frames(), SR as usize * 40);
    // Two 2 s heads of 16-bit stereo.
    assert_eq!(p.heads().len(), 2);
    assert_eq!(p.bytes(), 2 * 2 * SR as u64 * 2 * 2);
    // Short files stay whole in memory.
    let short = dir.path().join("short.wav");
    tone_wav(&short, SR as usize * 5, SR);
    let (p, _) = prepare(&short, SR, &[], PrepareOptions::default(), u64::MAX).unwrap();
    assert!(!p.is_streamed());
}

#[test]
fn seeks_with_a_full_pool_and_in_quick_succession() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 3, SR);
    let s = streamed(&path, &[]);
    let quiet = Playable::resident(bubble_engine::SampleBuffer::from_interleaved(
        SR,
        2,
        vec![0.0; SR as usize * 2],
    ));
    for voices in [1, 2] {
        let mut r = Rig::with_voices(voices);
        if voices > 1 {
            r.play(2, &quiet, PlayParams::default());
        }
        r.play(1, &s, PlayParams::default());
        r.render(1_000);
        // The pool is full: the seek's copy takes the other cart's slot, never the
        // voice that holds the stream.
        r.send(EngineCommand::Seek {
            cart: CartKey(1),
            frame: 50_000,
        });
        // Two more seeks before the first one's crossfade is over.
        let mut out = r.render(64);
        r.send(EngineCommand::Seek {
            cart: CartKey(1),
            frame: 70_000,
        });
        out.extend(r.render(32));
        r.send(EngineCommand::Seek {
            cart: CartKey(1),
            frame: 90_000,
        });
        out.extend(r.render(4_000));
        let last = out[out.len() - 2];
        assert!(
            last > 90_000.0 && last < 94_100.0,
            "{voices} voices: plays on after the last seek: {last}"
        );
        assert_eq!(out[out.len() - 4], last - 1.0);
        assert_eq!(r.h.status().active_voices(), 1);
    }
}

/// What Cart Setup does to a playing long file, against the clock: jumps, In and Out
/// moved while it loops, pauses and speed changes. A gap the reader could have
/// filled (an underrun, not the wait after a jump) is a bug.
#[test]
fn cart_setup_edits_keep_up_with_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.wav");
    ramp_wav(&path, SR as usize * 60, SR);
    let opts = PrepareOptions {
        stream_min_secs: 0.0,
        head_secs: 2.0,
    };
    let (a, _) = prepare(&path, SR, &[480_000], opts, u64::MAX).unwrap();
    let mut r = Rig::new();
    r.play(
        1,
        &a,
        PlayParams {
            start_frame: 480_000,
            end_frame: Some(720_000),
            looping: true,
            ..Default::default()
        },
    );
    let mut seed: u64 = std::env::var("STRESS_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(7);
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let mut last = String::new();
    for step in 0..60 {
        let before = r.underruns();
        let op = match rnd(6) {
            0 => {
                let frame = 480_000 + rnd(240_000) as usize;
                r.send(EngineCommand::Seek { cart: CartKey(1), frame });
                format!("seek {frame}")
            }
            1 => {
                let start = 400_000 + rnd(200_000) as usize;
                let end = start + 48_000 + rnd(400_000) as usize;
                r.send(EngineCommand::SetRegion { cart: CartKey(1), start, end });
                format!("region {start}..{end}")
            }
            2 => {
                // Up to 2 ×: at 4 × a jump to a place with no head can still come a
                // few milliseconds short while the reader catches up.
                let speed = [0.0, 0.5, 1.0, 1.5, 2.0][rnd(5) as usize];
                r.send(EngineCommand::SetSpeed { cart: CartKey(1), speed, ramp_frames: 240 });
                format!("speed {speed}")
            }
            3 => {
                let looping = rnd(4) != 0;
                r.send(EngineCommand::SetLoop { cart: CartKey(1), looping });
                format!("loop {looping}")
            }
            _ => "nothing".to_string(),
        };
        let n = 4_800 + rnd(20_000) as usize;
        // Real time: Cart Setup plays at up to 4 × speed.
        r.render_paced(n, 1.0);
        assert_eq!(r.underruns(), before, "step {step}: {op}, after {last}");
        last = op;
    }
    assert_eq!(r.underruns(), (0, 0), "no gaps");
}
