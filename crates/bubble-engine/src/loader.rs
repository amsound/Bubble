//! Decoding and preparation of audio files (never called on the audio thread).
//!
//! Files are decoded with symphonia to interleaved `f32` and, if needed, resampled
//! with rubato's sinc resampler to the engine rate (decision D-04).

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::buffer::SampleBuffer;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot open {path}: {source}")]
    Open {
        path: String,
        source: std::io::Error,
    },
    #[error("unsupported or unrecognised audio format: {0}")]
    Unsupported(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("file contains no audio")]
    Empty,
    #[error("resampling failed: {0}")]
    Resample(String),
    #[error(
        "too long to hold in memory: needs {} to load, {} of audio memory is free",
        gb(*needed),
        gb(*free)
    )]
    TooLarge { needed: u64, free: u64 },
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// Bytes one prepared frame of `channels` channels occupies (32-bit float).
pub fn frame_bytes(channels: u16) -> u64 {
    channels as u64 * 4
}

/// Audio decoded at its native rate.
#[derive(Debug)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved samples.
    pub samples: Vec<f32>,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// Decodes a whole file into memory.
pub fn decode_file(path: &Path) -> Result<DecodedAudio, LoadError> {
    decode_file_limited(path, 0, u64::MAX)
}

/// Decodes a whole file, refusing it when decoding (and resampling to
/// `engine_rate`, if non-zero) would need more than `max_bytes` at the peak. The
/// length is checked before decoding when the file states it, and as it decodes
/// otherwise.
pub fn decode_file_limited(
    path: &Path,
    engine_rate: u32,
    max_bytes: u64,
) -> Result<DecodedAudio, LoadError> {
    let file = File::open(path).map_err(|e| LoadError::Open {
        path: path.display().to_string(),
        source: e,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| LoadError::Unsupported(e.to_string()))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| LoadError::Unsupported("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| LoadError::Unsupported("track has no audio parameters".into()))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| LoadError::Unsupported(e.to_string()))?;

    let mut sample_rate = params.sample_rate.unwrap_or(0);
    let mut channels = params.channels.as_ref().map_or(0, |c| c.count()) as u16;
    if let Some(frames) = track.num_frames
        && sample_rate > 0
    {
        let needed = peak_bytes(frames, channels, sample_rate, engine_rate);
        if needed > max_bytes {
            return Err(LoadError::TooLarge {
                needed,
                free: max_bytes,
            });
        }
    }
    let mut samples: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                sample_rate = spec.rate();
                channels = spec.channels().count() as u16;
                scratch.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut scratch);
                let frames = (samples.len() + scratch.len()) as u64 / channels.max(1) as u64;
                let needed = peak_bytes(frames, channels, sample_rate, engine_rate);
                if needed > max_bytes {
                    return Err(LoadError::TooLarge {
                        needed,
                        free: max_bytes,
                    });
                }
                samples.extend_from_slice(&scratch);
            }
            // Corrupt packet: skip it (symphonia's documented recovery strategy).
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        }
    }
    if channels == 0 || sample_rate == 0 || samples.is_empty() {
        return Err(LoadError::Empty);
    }
    Ok(DecodedAudio {
        sample_rate,
        channels,
        samples,
    })
}

/// Memory needed at the peak of loading `frames` frames: the decoded audio plus,
/// when the rates differ, the resampled copy made from it.
fn peak_bytes(frames: u64, channels: u16, source_rate: u32, engine_rate: u32) -> u64 {
    let decoded = frames.saturating_mul(frame_bytes(channels));
    if engine_rate == 0 || engine_rate == source_rate || source_rate == 0 {
        decoded
    } else {
        decoded.saturating_add(decoded / source_rate as u64 * engine_rate as u64)
    }
}

/// Resamples interleaved audio to `target_rate` (high-quality sinc).
pub fn resample(audio: DecodedAudio, target_rate: u32) -> Result<DecodedAudio, LoadError> {
    if audio.sample_rate == target_rate {
        return Ok(audio);
    }
    let channels = audio.channels as usize;
    let frames_in = audio.frames();
    let ratio = target_rate as f64 / audio.sample_rate as f64;
    let params = SincInterpolationParameters::new(128, WindowFunction::Blackman2)
        .oversampling_factor(256)
        .interpolation(SincInterpolationType::Cubic);
    let mut rs = Async::<f32>::new_sinc(ratio, 1.0, &params, 1024, channels, FixedAsync::Input)
        .map_err(|e| LoadError::Resample(e.to_string()))?;
    let out_cap = rs.process_all_needed_output_len(frames_in);
    let mut out = vec![0.0f32; out_cap * channels];
    let input = InterleavedSlice::new(&audio.samples, channels, frames_in)
        .map_err(|e| LoadError::Resample(e.to_string()))?;
    let mut output = InterleavedSlice::new_mut(&mut out, channels, out_cap)
        .map_err(|e| LoadError::Resample(e.to_string()))?;
    let (_, frames_out) = rs
        .process_all_into_buffer(&input, &mut output, frames_in, None)
        .map_err(|e| LoadError::Resample(e.to_string()))?;
    out.truncate(frames_out * channels);
    Ok(DecodedAudio {
        sample_rate: target_rate,
        channels: audio.channels,
        samples: out,
    })
}

/// Format of a file before preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
}

/// Decodes and prepares a file for the mixer running at `engine_rate`.
pub fn load_for_engine(path: &Path, engine_rate: u32) -> Result<Arc<SampleBuffer>, LoadError> {
    load_for_engine_with_info(path, engine_rate).map(|(b, _)| b)
}

/// Like [`load_for_engine`], also returning the source format.
pub fn load_for_engine_with_info(
    path: &Path,
    engine_rate: u32,
) -> Result<(Arc<SampleBuffer>, SourceInfo), LoadError> {
    load_for_engine_limited(path, engine_rate, u64::MAX)
}

/// Like [`load_for_engine_with_info`], refusing files that would need more than
/// `max_bytes` while loading (see [`decode_file_limited`]).
pub fn load_for_engine_limited(
    path: &Path,
    engine_rate: u32,
    max_bytes: u64,
) -> Result<(Arc<SampleBuffer>, SourceInfo), LoadError> {
    let decoded = decode_file_limited(path, engine_rate, max_bytes)?;
    let info = SourceInfo {
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
        frames: decoded.frames() as u64,
    };
    let decoded = resample(decoded, engine_rate)?;
    let buf = SampleBuffer::from_interleaved(engine_rate, decoded.channels, decoded.samples);
    Ok((buf.shared(), info))
}

/// Whether `path` would load without decoding or resampling at `engine_rate`: a
/// WAV file already at that rate. Unreadable files count as not needing it.
pub fn needs_wav_conversion(path: &Path, engine_rate: u32) -> bool {
    let is_wav = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("wave"));
    if !is_wav {
        return true;
    }
    match hound::WavReader::open(path) {
        Ok(r) => r.spec().sample_rate != engine_rate,
        // hound reads plain PCM and float WAVs; anything else (e.g. compressed WAV)
        // is converted too, as long as symphonia can decode it.
        Err(_) => true,
    }
}

/// Decodes `src`, resamples it to `engine_rate` and writes it to `dst` as 32-bit
/// float WAV (no clipping of decoded overs). Written to a temporary name first, so
/// a failed conversion never leaves a partial file under `dst`. Returns the source
/// format.
pub fn convert_to_wav(src: &Path, dst: &Path, engine_rate: u32) -> Result<SourceInfo, LoadError> {
    let decoded = decode_file(src)?;
    let info = SourceInfo {
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
        frames: decoded.frames() as u64,
    };
    let audio = resample(decoded, engine_rate)?;
    let io = |e: hound::Error| LoadError::Open {
        path: dst.display().to_string(),
        source: match e {
            hound::Error::IoError(e) => e,
            other => std::io::Error::other(other.to_string()),
        },
    };
    let tmp = dst.with_file_name(format!(
        ".{}.part",
        dst.file_name().unwrap_or_default().to_string_lossy()
    ));
    let spec = hound::WavSpec {
        channels: audio.channels,
        sample_rate: engine_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let result = (|| {
        let mut w = hound::WavWriter::create(&tmp, spec).map_err(io)?;
        for s in &audio.samples {
            w.write_sample(*s).map_err(io)?;
        }
        w.finalize().map_err(io)?;
        std::fs::rename(&tmp, dst).map_err(|e| LoadError::Open {
            path: dst.display().to_string(),
            source: e,
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| info)
}
