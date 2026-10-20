//! Immutable, shareable audio data prepared for the mixer.

use std::sync::Arc;

/// 16-bit samples are scaled by this to −1.0 … +1.0 (exactly as decoders do).
const I16_SCALE: f32 = 1.0 / 32768.0;

/// Interleaved samples, either 32-bit float or 16-bit integer.
#[derive(Debug)]
enum Data {
    F32(Box<[f32]>),
    /// 16-bit sources at the engine rate are kept as they are: exact, and half the
    /// memory of float.
    I16(Box<[i16]>),
}

/// Interleaved audio at the engine sample rate. Created off the audio thread
/// (see [`crate::loader`]) and shared with voices through `Arc`.
#[derive(Debug)]
pub struct SampleBuffer {
    sample_rate: u32,
    channels: u16,
    frames: usize,
    data: Data,
}

/// The samples of one frame.
#[derive(Clone, Copy, Debug)]
pub enum Frame<'a> {
    F32(&'a [f32]),
    I16(&'a [i16]),
}

impl Frame<'_> {
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Frame::F32(s) => s.len(),
            Frame::I16(s) => s.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Channel `c` as −1.0 … +1.0.
    #[inline]
    pub fn get(&self, c: usize) -> f32 {
        match self {
            Frame::F32(s) => s[c],
            Frame::I16(s) => s[c] as f32 * I16_SCALE,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        (0..self.len()).map(|c| self.get(c))
    }
}

impl SampleBuffer {
    /// Builds a buffer from interleaved samples. Trailing samples that do not form a
    /// whole frame are discarded.
    pub fn from_interleaved(sample_rate: u32, channels: u16, mut data: Vec<f32>) -> Self {
        assert!(channels > 0, "channel count must be positive");
        let frames = data.len() / channels as usize;
        data.truncate(frames * channels as usize);
        Self {
            sample_rate,
            channels,
            frames,
            data: Data::F32(data.into_boxed_slice()),
        }
    }

    /// Like [`Self::from_interleaved`], holding 16-bit samples as they are.
    pub fn from_interleaved_i16(sample_rate: u32, channels: u16, mut data: Vec<i16>) -> Self {
        assert!(channels > 0, "channel count must be positive");
        let frames = data.len() / channels as usize;
        data.truncate(frames * channels as usize);
        Self {
            sample_rate,
            channels,
            frames,
            data: Data::I16(data.into_boxed_slice()),
        }
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate as f64
    }

    /// Number of interleaved samples (frames × channels).
    pub fn len(&self) -> usize {
        self.frames * self.channels as usize
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    /// Memory held by the samples.
    pub fn bytes(&self) -> u64 {
        match &self.data {
            Data::F32(d) => d.len() as u64 * 4,
            Data::I16(d) => d.len() as u64 * 2,
        }
    }

    /// Whether the samples are held as 16-bit integers.
    pub fn is_16_bit(&self) -> bool {
        matches!(self.data, Data::I16(_))
    }

    /// Interleaved sample `i` as −1.0 … +1.0. `i` must be `< len()`.
    #[inline]
    pub fn sample(&self, i: usize) -> f32 {
        match &self.data {
            Data::F32(d) => d[i],
            Data::I16(d) => d[i] as f32 * I16_SCALE,
        }
    }

    /// The samples of one frame. `frame` must be `< frames()`.
    #[inline]
    pub fn frame(&self, frame: usize) -> Frame<'_> {
        let ch = self.channels as usize;
        let r = frame * ch..frame * ch + ch;
        match &self.data {
            Data::F32(d) => Frame::F32(&d[r]),
            Data::I16(d) => Frame::I16(&d[r]),
        }
    }

    /// Absolute peak over all samples (linear).
    pub fn peak(&self) -> f32 {
        (0..self.len()).fold(0.0f32, |m, i| m.max(self.sample(i).abs()))
    }
}
