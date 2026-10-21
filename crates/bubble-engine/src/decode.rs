//! Sequential, seekable decoding at the engine rate (never on the audio thread).
//!
//! [`StreamDecoder`] produces a file's audio from any frame onwards, resampled to the
//! engine rate as it goes. The disk reader ([`crate::stream`]) uses it to fill voice
//! rings, and the loader uses it to prepare the pre-roll heads, so both produce the
//! same samples for the same frame.
//!
//! Resampled positions line up exactly with a whole-file resample: decoding restarts
//! at a source frame whose engine-rate position is a whole number (a multiple of the
//! rate ratio's denominator), a little before the frame asked for so the sinc filter
//! has settled, and the frames before it are dropped.

use std::fs::File;
use std::path::Path;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

use crate::loader::LoadError;

/// Resampler input chunk (source frames).
const CHUNK: usize = 1024;
/// Source frames decoded before a resampled seek target, so the filter has settled.
const WARMUP: u64 = 512;
/// Largest rate-ratio denominator for exact alignment; beyond it (unusual rate pairs)
/// a seek starts a little early without exact alignment.
const MAX_DEN: u64 = 1 << 16;

/// An opened file: format reader, decoder and the facts about its audio track.
pub(crate) struct Opened {
    pub format: Box<dyn FormatReader>,
    pub decoder: Box<dyn AudioDecoder>,
    pub track_id: u32,
    pub sample_rate: u32,
    pub channels: u16,
    /// Length in source frames, when the file states it.
    pub frames: Option<u64>,
}

pub(crate) fn open(path: &Path) -> Result<Opened, LoadError> {
    let file = File::open(path).map_err(|e| LoadError::Open {
        path: path.display().to_string(),
        source: e,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
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
    let frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| LoadError::Unsupported("track has no audio parameters".into()))?
        .clone();
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| LoadError::Unsupported(e.to_string()))?;
    Ok(Opened {
        format,
        decoder,
        track_id,
        sample_rate: params.sample_rate.unwrap_or(0),
        channels: params.channels.as_ref().map_or(0, |c| c.count()) as u16,
        frames,
    })
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

struct Resampling {
    rs: Async<f32>,
    /// Engine rate / source rate as `num / den` in lowest terms.
    num: u64,
    den: u64,
    delay: usize,
    out: Vec<f32>,
    /// Zero chunks still to feed after the end of the file, to flush the filter.
    flush: u8,
}

/// Decodes a file from any engine frame onwards at the engine rate.
pub(crate) struct StreamDecoder {
    file: Opened,
    channels: usize,
    rs: Option<Resampling>,
    /// Decoded source audio not yet resampled or handed out (interleaved).
    pending: Vec<f32>,
    /// Source frames still to drop from the next decoded packets (after a seek).
    drop_src: u64,
    /// Engine frames still to drop from the output (filter delay and warm-up).
    drop_out: u64,
    /// Source frame of the next decoded packet, when known.
    src_pos: Option<u64>,
    eof: bool,
    /// Engine-rate output decoded but not handed out yet (interleaved).
    ready: Vec<f32>,
    scratch: Vec<f32>,
    /// Whether the next read continues from the start of the file without a seek.
    fresh: bool,
}

impl StreamDecoder {
    pub fn open(path: &Path, engine_rate: u32) -> Result<Self, LoadError> {
        let file = open(path)?;
        if file.sample_rate == 0 || file.channels == 0 {
            return Err(LoadError::Unsupported(
                "unknown sample rate or channels".into(),
            ));
        }
        let channels = file.channels as usize;
        let rs = if file.sample_rate != engine_rate {
            let g = gcd(engine_rate as u64, file.sample_rate as u64);
            let ratio = engine_rate as f64 / file.sample_rate as f64;
            let params = SincInterpolationParameters::new(128, WindowFunction::Blackman2)
                .oversampling_factor(256)
                .interpolation(SincInterpolationType::Cubic);
            let rs =
                Async::<f32>::new_sinc(ratio, 1.0, &params, CHUNK, channels, FixedAsync::Input)
                    .map_err(|e| LoadError::Resample(e.to_string()))?;
            let delay = rs.output_delay();
            let max = rs.output_frames_max();
            Some(Resampling {
                rs,
                num: engine_rate as u64 / g,
                den: file.sample_rate as u64 / g,
                delay,
                out: vec![0.0; max * channels],
                flush: 2,
            })
        } else {
            None
        };
        let drop_out = rs.as_ref().map_or(0, |r| r.delay as u64);
        Ok(StreamDecoder {
            file,
            channels,
            rs,
            pending: Vec::new(),
            drop_src: 0,
            drop_out,
            src_pos: Some(0),
            eof: false,
            ready: Vec::new(),
            scratch: Vec::new(),
            fresh: true,
        })
    }

    pub fn channels(&self) -> u16 {
        self.channels as u16
    }

    /// Whether the output is resampled (and so not exactly the file's samples).
    pub fn resampled(&self) -> bool {
        self.rs.is_some()
    }

    /// Positions the decoder so the next [`Self::read`] starts at engine frame `frame`.
    pub fn seek(&mut self, frame: u64) -> Result<(), LoadError> {
        let (src, drop_out) = match &mut self.rs {
            None => (frame, 0),
            Some(r) => {
                r.rs.reset();
                r.flush = 2;
                let want = frame * r.den / r.num;
                let early = want.saturating_sub(WARMUP);
                let src = if r.den <= MAX_DEN {
                    early / r.den * r.den
                } else {
                    early
                };
                let at = src * r.num / r.den;
                (src, frame - at.min(frame) + r.delay as u64)
            }
        };
        self.pending.clear();
        self.ready.clear();
        self.eof = false;
        self.drop_out = drop_out;
        if src == 0 && self.fresh {
            self.drop_src = 0;
            return Ok(());
        }
        self.fresh = false;
        let track_id = self.file.track_id;
        let seeked = self
            .file
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(src as i64),
                    track_id,
                },
            )
            .map_err(|e| LoadError::Decode(format!("seek: {e}")))?;
        self.file.decoder.reset();
        // The first packet's timestamp says where decoding resumes; an accurate seek
        // lands at or before `src`, and the frames before it are dropped.
        let _ = seeked;
        self.src_pos = None;
        self.drop_src = src;
        Ok(())
    }

    /// Decodes the next packet into `pending` (after dropping frames before a seek
    /// target). Returns false at the end of the file.
    fn decode_packet(&mut self) -> Result<bool, LoadError> {
        self.fresh = false;
        loop {
            let packet = match self.file.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(e) => return Err(LoadError::Decode(e.to_string())),
            };
            if packet.track_id != self.file.track_id {
                continue;
            }
            let pts = packet.pts.get().max(0) as u64;
            let buf = match self.file.decoder.decode(&packet) {
                Ok(b) => b,
                // Corrupt packet: skip it (symphonia's documented recovery strategy).
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(LoadError::Decode(e.to_string())),
            };
            if buf.spec().channels().count() != self.channels {
                return Err(LoadError::Decode("channel count changed mid-file".into()));
            }
            self.scratch.resize(buf.samples_interleaved(), 0.0);
            buf.copy_to_slice_interleaved(&mut self.scratch);
            let frames = self.scratch.len() / self.channels;
            let start = self.src_pos.unwrap_or(pts);
            self.src_pos = Some(start + frames as u64);
            // Frames before the seek target (an accurate seek lands at or before it).
            let skip = (self.drop_src.saturating_sub(start) as usize).min(frames);
            self.pending
                .extend_from_slice(&self.scratch[skip * self.channels..]);
            return Ok(true);
        }
    }

    /// Appends up to `max` engine-rate frames to `out`; fewer only at the end of the
    /// file. Returns the frames appended.
    pub fn read(&mut self, out: &mut Vec<f32>, max: usize) -> Result<usize, LoadError> {
        let ch = self.channels;
        let mut produced = 0;
        while produced < max {
            if !self.ready.is_empty() {
                let n = (self.ready.len() / ch).min(max - produced);
                out.extend_from_slice(&self.ready[..n * ch]);
                self.ready.drain(..n * ch);
                produced += n;
                continue;
            }
            if !self.fill_ready()? {
                break;
            }
        }
        Ok(produced)
    }

    /// Makes more output ready. Returns false at the end of the file.
    fn fill_ready(&mut self) -> Result<bool, LoadError> {
        let ch = self.channels;
        if self.rs.is_none() {
            if self.eof || !self.decode_packet()? {
                self.eof = true;
                return Ok(false);
            }
            std::mem::swap(&mut self.ready, &mut self.pending);
            return Ok(true);
        }
        // Resampled: feed whole chunks; at the end, pad with silence to flush the filter.
        let need = self.rs.as_ref().map_or(CHUNK, |r| r.rs.input_frames_next());
        while self.pending.len() / ch < need && !self.eof {
            if !self.decode_packet()? {
                self.eof = true;
            }
        }
        let r = self.rs.as_mut().expect("resampler");
        if self.pending.len() / ch < need {
            if r.flush == 0 {
                return Ok(false);
            }
            r.flush -= 1;
            self.pending.resize(need * ch, 0.0);
        }
        let out_cap = r.out.len() / ch;
        let input = InterleavedSlice::new(&self.pending[..need * ch], ch, need)
            .map_err(|e| LoadError::Resample(e.to_string()))?;
        let mut output = InterleavedSlice::new_mut(&mut r.out, ch, out_cap)
            .map_err(|e| LoadError::Resample(e.to_string()))?;
        let (used, made) =
            r.rs.process_into_buffer(&input, &mut output, None)
                .map_err(|e| LoadError::Resample(e.to_string()))?;
        self.pending.drain(..used * ch);
        let skip = (self.drop_out as usize).min(made);
        self.drop_out -= skip as u64;
        self.ready.extend_from_slice(&r.out[skip * ch..made * ch]);
        Ok(true)
    }
}
