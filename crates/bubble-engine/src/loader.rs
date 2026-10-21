//! Decoding and preparation of audio files (never called on the audio thread).
//!
//! Files are decoded with symphonia to interleaved `f32` and, if needed, resampled
//! with rubato's sinc resampler to the engine rate (decision D-04). Long PCM files
//! are not decoded whole: [`prepare`] keeps a pre-roll head in memory and the rest is
//! streamed while playing (decision D-37).

use std::path::Path;
use std::sync::Arc;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use symphonia::core::errors::Error as SymError;

use crate::buffer::SampleBuffer;
use crate::decode::StreamDecoder;
use crate::overview::{Overview, OverviewBuilder};
use crate::playable::{Head, MAX_CHANNELS, Playable};

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
    let crate::decode::Opened {
        mut format,
        mut decoder,
        track_id,
        sample_rate,
        channels,
        frames,
    } = crate::decode::open(path)?;
    let (mut sample_rate, mut channels) = (sample_rate, channels);
    if let Some(frames) = frames
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

/// Whether every sample is exactly a 16-bit value (16-bit PCM, FLAC or ALAC). Lossy
/// and 24-bit audio fails at its first sample or so, so this is cheap when false.
fn is_16_bit(samples: &[f32]) -> bool {
    samples.iter().all(|x| {
        let v = x * 32768.0;
        v == v.round() && (-32768.0..=32767.0).contains(&v)
    })
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
    let resampled = decoded.sample_rate != engine_rate;
    let decoded = resample(decoded, engine_rate)?;
    // 16-bit (or narrower) sources not resampled are held as 16-bit: exact, and half
    // the memory. Resampled, 24-bit, float and lossy audio stays 32-bit float.
    let buf = if !resampled && is_16_bit(&decoded.samples) {
        let ints = decoded
            .samples
            .iter()
            .map(|x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect();
        SampleBuffer::from_interleaved_i16(engine_rate, decoded.channels, ints)
    } else {
        SampleBuffer::from_interleaved(engine_rate, decoded.channels, decoded.samples)
    };
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

/// How [`prepare`] holds audio.
#[derive(Clone, Copy, Debug)]
pub struct PrepareOptions {
    /// Files at least this long (seconds) are streamed; shorter ones are held whole.
    pub stream_min_secs: f64,
    /// Length of each pre-roll head (seconds).
    pub head_secs: f64,
}

impl Default for PrepareOptions {
    fn default() -> Self {
        PrepareOptions {
            stream_min_secs: 30.0,
            head_secs: 2.0,
        }
    }
}

/// Whether files like `path` can be streamed: uncompressed formats, where a seek is
/// exact and cheap. Others (MP3, AAC, FLAC, Ogg…) are held whole in memory.
pub fn is_streamable_format(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["wav", "wave", "aif", "aiff", "aifc"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// Engine frame of source frame `frame` (rounded as the control layer rounds trims).
pub fn engine_frame(frame: u64, source_rate: u32, engine_rate: u32) -> usize {
    if source_rate == engine_rate || source_rate == 0 {
        frame as usize
    } else {
        (frame as f64 * engine_rate as f64 / source_rate as f64).round() as usize
    }
}

/// Prepares a file for the mixer at `engine_rate`. Short files (and formats that
/// are not streamed) are decoded whole, refused beyond `max_bytes`; long ones keep a
/// head in memory at each of `starts` (source frames; the file start is always
/// included) and stream the rest.
pub fn prepare(
    path: &Path,
    engine_rate: u32,
    starts: &[u64],
    opts: PrepareOptions,
    max_bytes: u64,
) -> Result<(Arc<Playable>, SourceInfo), LoadError> {
    let probe = crate::decode::open(path)?;
    let (rate, channels, frames) = (probe.sample_rate, probe.channels, probe.frames);
    drop(probe);
    let long = frames.is_some_and(|f| rate > 0 && f as f64 / rate as f64 >= opts.stream_min_secs);
    if !(long && is_streamable_format(path) && (1..=MAX_CHANNELS as u16).contains(&channels)) {
        let (buf, info) = load_for_engine_limited(path, engine_rate, max_bytes)?;
        let buf = Arc::try_unwrap(buf).expect("fresh buffer");
        return Ok((Playable::resident(buf), info));
    }
    let src_frames = frames.unwrap_or(0);
    let info = SourceInfo {
        sample_rate: rate,
        channels,
        frames: src_frames,
    };
    let total = if rate == engine_rate {
        src_frames as usize
    } else {
        (src_frames as f64 * engine_rate as f64 / rate as f64).ceil() as usize
    };
    let head_len = ((opts.head_secs * engine_rate as f64) as usize).max(1);
    let mut at: Vec<usize> = std::iter::once(0)
        .chain(starts.iter().map(|s| engine_frame(*s, rate, engine_rate)))
        .filter(|s| *s < total)
        .collect();
    at.sort_unstable();
    at.dedup();
    let mut heads = Vec::with_capacity(at.len());
    let mut bytes = 0u64;
    for start in at {
        let head = read_head(path, engine_rate, start, head_len)?;
        bytes += head.buffer.bytes();
        if bytes > max_bytes {
            return Err(LoadError::TooLarge {
                needed: bytes,
                free: max_bytes,
            });
        }
        heads.push(head);
    }
    Ok((
        Playable::streamed(path, engine_rate, channels, total, heads),
        info,
    ))
}

/// A head of `len` engine frames from `start`, decoded the way the disk reader does.
pub fn read_head(
    path: &Path,
    engine_rate: u32,
    start: usize,
    len: usize,
) -> Result<Head, LoadError> {
    let mut d = StreamDecoder::open(path, engine_rate)?;
    d.seek(start as u64)?;
    let mut samples = Vec::with_capacity(len * d.channels() as usize);
    d.read(&mut samples, len)?;
    let buffer = if !d.resampled() && is_16_bit(&samples) {
        let ints = samples
            .iter()
            .map(|x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect();
        SampleBuffer::from_interleaved_i16(engine_rate, d.channels(), ints)
    } else {
        SampleBuffer::from_interleaved(engine_rate, d.channels(), samples)
    };
    Ok(Head {
        start,
        buffer: buffer.shared(),
    })
}

/// Reads a whole file once (at its own rate, without holding it) for its waveform
/// overview.
pub fn scan_overview(path: &Path) -> Result<Overview, LoadError> {
    let crate::decode::Opened {
        mut format,
        mut decoder,
        track_id,
        sample_rate,
        channels,
        frames,
    } = crate::decode::open(path)?;
    let frames = match frames {
        Some(f) => f,
        // Unknown length: count it first.
        None => decode_file(path)?.frames() as u64,
    };
    let mut b = OverviewBuilder::new(frames, sample_rate, channels);
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
                if buf.spec().channels().count() != channels as usize {
                    continue;
                }
                scratch.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut scratch);
                b.push(&scratch);
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(LoadError::Decode(e.to_string())),
        }
    }
    Ok(b.finish())
}
