//! Loader tests against the reference package's test signals (TestFiles/).

use std::path::PathBuf;

use bubble_engine::loader::{decode_file, load_for_engine, resample};

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

fn dbfs(x: f32) -> f32 {
    20.0 * x.log10()
}

#[test]
fn decodes_wav_with_expected_level_and_format() {
    if missing_test_files() {
        return;
    }
    // 1 kHz tone, 16-bit, 48 kHz, both channels, −18 dBFS.
    let a = decode_file(&test_file("1kHz_16_48_AB_M18.wav")).unwrap();
    assert_eq!(a.sample_rate, 48_000);
    assert_eq!(a.channels, 2);
    let peak = a.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((dbfs(peak) + 18.0).abs() < 0.1, "peak {} dBFS", dbfs(peak));
}

#[test]
fn decodes_mono_and_multichannel() {
    if missing_test_files() {
        return;
    }
    // "_A_" files are stereo with signal on channel A only; the DTMF files are true mono.
    let a_only = decode_file(&test_file("1kHz_16_44_A_0.wav")).unwrap();
    assert_eq!((a_only.channels, a_only.sample_rate), (2, 44_100));
    let right_peak = a_only
        .samples
        .iter()
        .skip(1)
        .step_by(2)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(right_peak < 1e-4, "channel B silent");
    let mono = decode_file(&test_file("DTMF1_48_A_M18.wav")).unwrap();
    assert_eq!((mono.channels, mono.sample_rate), (1, 48_000));
    let six = decode_file(&test_file("Ident_6Ch.wav")).unwrap();
    assert_eq!(six.channels, 6);
}

#[test]
fn resamples_to_engine_rate_preserving_duration_and_level() {
    if missing_test_files() {
        return;
    }
    let src = decode_file(&test_file("1kHz_16_44_AB_0.wav")).unwrap();
    let secs = src.frames() as f64 / src.sample_rate as f64;
    let out = resample(src, 48_000).unwrap();
    assert_eq!(out.sample_rate, 48_000);
    let out_secs = out.frames() as f64 / 48_000.0;
    assert!((out_secs - secs).abs() < 0.002, "{secs} vs {out_secs}");
    // Level preserved within 0.2 dB (0 dBFS tone; ignore filter edges).
    let mid = &out.samples[out.samples.len() / 4..out.samples.len() * 3 / 4];
    let peak = mid.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(dbfs(peak).abs() < 0.2, "peak {} dBFS", dbfs(peak));
}

#[test]
fn every_test_wav_loads_for_engine() {
    if missing_test_files() {
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../TestFiles");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
            let buf =
                load_for_engine(&p, 48_000).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert!(buf.frames() > 0, "{}", p.display());
            assert_eq!(buf.sample_rate(), 48_000);
            n += 1;
        }
    }
    assert!(n > 50, "expected the reference corpus, found {n} files");
}

#[test]
fn missing_and_non_audio_files_error_cleanly() {
    if missing_test_files() {
        return;
    }
    assert!(decode_file(&test_file("does-not-exist.wav")).is_err());
    // The .pk peak file is not audio.
    assert!(decode_file(&test_file("1kHz_16_44_A_0.pk")).is_err());
}

#[test]
fn refuses_files_over_the_memory_limit() {
    if missing_test_files() {
        return;
    }
    use bubble_engine::loader::{LoadError, load_for_engine_limited};
    let path = test_file("1kHz_16_44_AB_M18.wav");
    // Resampling 44.1 → 48 kHz needs the decoded and resampled copies at once.
    let err = load_for_engine_limited(&path, 48_000, 10_000).unwrap_err();
    assert!(matches!(err, LoadError::TooLarge { .. }), "{err}");
    assert!(err.to_string().contains("too long to hold in memory"));
    let (buf, _) = load_for_engine_limited(&path, 48_000, u64::MAX).unwrap();
    assert_eq!(buf.sample_rate(), 48_000);
}

#[test]
fn converts_to_wav_at_the_engine_rate() {
    if missing_test_files() {
        return;
    }
    use bubble_engine::loader::{convert_to_wav, needs_wav_conversion};
    let dir = tempfile_dir();
    let src = test_file("1kHz_16_44_AB_M18.wav");
    assert!(needs_wav_conversion(&src, 48_000), "44.1 kHz WAV");
    assert!(!needs_wav_conversion(
        &test_file("1kHz_16_48_AB_M18.wav"),
        48_000
    ));
    let dst = dir.join("tone.wav");
    let info = convert_to_wav(&src, &dst, 48_000).unwrap();
    assert_eq!((info.sample_rate, info.channels), (44_100, 2));
    assert!(
        !needs_wav_conversion(&dst, 48_000),
        "converted file is ready"
    );
    let a = decode_file(&dst).unwrap();
    assert_eq!(a.sample_rate, 48_000);
    let expect = info.frames as f64 * 48_000.0 / 44_100.0;
    assert!(
        (a.frames() as f64 - expect).abs() < 2.0,
        "{} vs {expect}",
        a.frames()
    );
    let peak = a.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        (dbfs(peak) + 18.0).abs() < 0.2,
        "level kept: {} dBFS",
        dbfs(peak)
    );
    assert!(
        std::fs::read_dir(&dir).unwrap().count() == 1,
        "no temporary file left"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn tempfile_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("bubble-wav-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
