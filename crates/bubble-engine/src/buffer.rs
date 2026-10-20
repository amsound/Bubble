//! Immutable, shareable audio data prepared for the mixer.

use std::sync::Arc;

/// Interleaved `f32` audio at the engine sample rate. Created off the audio thread
/// (see [`crate::loader`]) and shared with voices through `Arc`.
#[derive(Debug)]
pub struct SampleBuffer {
    sample_rate: u32,
    channels: u16,
    frames: usize,
    data: Box<[f32]>,
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
            data: data.into_boxed_slice(),
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

    pub fn samples(&self) -> &[f32] {
        &self.data
    }

    /// The samples of one frame. `frame` must be `< frames()`.
    #[inline]
    pub fn frame(&self, frame: usize) -> &[f32] {
        let ch = self.channels as usize;
        &self.data[frame * ch..frame * ch + ch]
    }

    /// Absolute peak over all samples (linear).
    pub fn peak(&self) -> f32 {
        self.data.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }
}
