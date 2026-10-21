//! Waveform overview of a whole file: min/max envelope plus where the audio starts and
//! ends. Computed once per file off the audio thread (by scanning the file, see
//! [`crate::loader::scan_overview`]), since streamed files are never whole in memory.

use crate::buffer::SampleBuffer;

/// Most buckets in an overview.
pub const MAX_BUCKETS: usize = 16_384;
/// Level that counts as audio for [`Overview::audio_bounds`] ("Top and tail").
pub const AUDIO_THRESHOLD_DB: f32 = -40.0;

/// Min/max envelope of the signal in fixed-size buckets (the channel with the largest
/// excursion, so stereo content is never hidden).
#[derive(Clone, Debug)]
pub struct Overview {
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    pub duration_s: f64,
    /// First and last frame at or above [`AUDIO_THRESHOLD_DB`].
    loud: Option<(u64, u64)>,
    sample_rate: u32,
}

impl Overview {
    /// A flat overview of `duration_s`, shown while the real one is computed.
    pub fn placeholder(duration_s: f64) -> Self {
        Overview {
            min: vec![0.0],
            max: vec![0.0],
            duration_s,
            loud: None,
            sample_rate: 1,
        }
    }

    /// First and last time (seconds) where the signal reaches [`AUDIO_THRESHOLD_DB`].
    pub fn audio_bounds(&self) -> Option<(f64, f64)> {
        let rate = self.sample_rate.max(1) as f64;
        self.loud
            .map(|(a, b)| (a as f64 / rate, (b + 1) as f64 / rate))
    }

    /// Overview of audio held in memory.
    pub fn from_buffer(buf: &SampleBuffer) -> Self {
        let mut b = OverviewBuilder::new(buf.frames() as u64, buf.sample_rate(), buf.channels());
        let mut frame = Vec::with_capacity(buf.channels() as usize);
        for f in 0..buf.frames() {
            frame.clear();
            frame.extend(buf.frame(f).iter());
            b.push(&frame);
        }
        b.finish()
    }
}

/// Builds an [`Overview`] from interleaved samples fed in order.
pub struct OverviewBuilder {
    min: Vec<f32>,
    max: Vec<f32>,
    per: f64,
    frames: u64,
    pos: u64,
    channels: usize,
    rate: u32,
    threshold: f32,
    loud: Option<(u64, u64)>,
}

impl OverviewBuilder {
    /// For a file of `frames` frames (as stated by its header).
    pub fn new(frames: u64, rate: u32, channels: u16) -> Self {
        let n = frames.max(1);
        let buckets = n.min(MAX_BUCKETS as u64) as usize;
        OverviewBuilder {
            min: vec![f32::MAX; buckets],
            max: vec![f32::MIN; buckets],
            per: n as f64 / buckets as f64,
            frames: n,
            pos: 0,
            channels: channels.max(1) as usize,
            rate,
            threshold: 10f32.powf(AUDIO_THRESHOLD_DB / 20.0),
            loud: None,
        }
    }

    /// Adds interleaved samples (whole frames).
    pub fn push(&mut self, samples: &[f32]) {
        let last = self.min.len() - 1;
        for frame in samples.chunks_exact(self.channels) {
            let b = ((self.pos as f64 / self.per) as usize).min(last);
            let (mut lo, mut hi) = (self.min[b], self.max[b]);
            let mut loud = false;
            for &s in frame {
                lo = lo.min(s);
                hi = hi.max(s);
                loud |= s.abs() >= self.threshold;
            }
            self.min[b] = lo;
            self.max[b] = hi;
            if loud {
                let first = self.loud.map_or(self.pos, |(a, _)| a);
                self.loud = Some((first, self.pos));
            }
            self.pos += 1;
        }
    }

    pub fn finish(mut self) -> Overview {
        // Buckets the file never reached (its header overstated the length) are silent.
        for (lo, hi) in self.min.iter_mut().zip(self.max.iter_mut()) {
            if *lo > *hi {
                *lo = 0.0;
                *hi = 0.0;
            }
        }
        let frames = self.pos.max(1).min(self.frames.max(self.pos));
        Overview {
            min: self.min,
            max: self.max,
            duration_s: frames as f64 / self.rate.max(1) as f64,
            loud: self.loud,
            sample_rate: self.rate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_and_bounds() {
        // 1 s of silence, 1 s at 0.5, 1 s of silence (mono, 1 kHz rate for brevity).
        let mut v = vec![0.0f32; 1000];
        v.extend(std::iter::repeat_n(0.5, 1000));
        v.extend(std::iter::repeat_n(0.0, 1000));
        let buf = SampleBuffer::from_interleaved(1000, 1, v);
        let p = Overview::from_buffer(&buf);
        assert_eq!(p.min.len(), 3000);
        assert!((p.duration_s - 3.0).abs() < 1e-9);
        let (a, b) = p.audio_bounds().unwrap();
        assert!((a - 1.0).abs() < 1e-9 && (b - 2.0).abs() < 1e-9, "{a} {b}");
        let quiet = Overview::from_buffer(&SampleBuffer::from_interleaved(1000, 1, vec![0.0; 10]));
        assert!(quiet.audio_bounds().is_none());
    }

    #[test]
    fn long_files_fold_into_the_bucket_limit() {
        let n = MAX_BUCKETS * 3 + 7;
        let mut b = OverviewBuilder::new(n as u64, 48_000, 2);
        let mut v = vec![0.0f32; n * 2];
        v[2 * (n - 1)] = 0.9;
        b.push(&v);
        let o = b.finish();
        assert_eq!(o.max.len(), MAX_BUCKETS);
        assert_eq!(*o.max.last().unwrap(), 0.9);
    }
}
