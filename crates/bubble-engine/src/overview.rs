//! Waveform overview of a whole file: min/max envelope plus where the audio starts and
//! ends. Computed once per file off the audio thread (by scanning the file, see
//! [`crate::loader::scan_overview`]), since streamed files are never whole in memory.

use crate::buffer::SampleBuffer;

/// Most buckets in an overview.
pub const MAX_BUCKETS: usize = 16_384;
/// Frames per bucket of the fine envelope, for zoomed-in views (a percussive hit
/// stays a shape, not blocks).
pub const FINE_FRAMES: u64 = 16;
/// Most buckets in the fine envelope (4 MB): a longer file gets wider fine buckets.
pub const MAX_FINE: u64 = 1 << 20;
/// Level that counts as audio for [`Overview::audio_bounds`] ("Top and tail").
pub const AUDIO_THRESHOLD_DB: f32 = -40.0;

/// Min/max envelope of the signal in fixed-size buckets (the channel with the largest
/// excursion, so stereo content is never hidden).
#[derive(Clone, Debug)]
pub struct Overview {
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    /// The same envelope, finer: (min, max) as 16-bit, `fine_per` frames each.
    fine: Vec<(i16, i16)>,
    fine_per: f64,
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
            fine: Vec::new(),
            fine_per: 1.0,
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

    /// The lowest and highest sample between `t0` and `t1` (seconds), from the
    /// finest envelope that has at least one bucket there. Narrower than a bucket (a
    /// deep zoom), the envelope is interpolated between bucket centres, so the shape
    /// stays smooth instead of stepping.
    pub fn envelope(&self, t0: f64, t1: f64) -> (f32, f32) {
        let rate = self.sample_rate.max(1) as f64;
        let coarse_per = self.duration_s * rate / self.max.len().max(1) as f64;
        let (f0, f1) = (t0 * rate, t1 * rate);
        if !self.fine.is_empty() && f1 - f0 < coarse_per * 2.0 {
            let v = |i: usize| {
                let (lo, hi) = self.fine[i.min(self.fine.len() - 1)];
                (lo as f32 / 32767.0, hi as f32 / 32767.0)
            };
            return Self::span(f0, f1, self.fine_per, self.fine.len(), v);
        }
        let v = |i: usize| {
            let i = i.min(self.max.len() - 1);
            (self.min[i], self.max[i])
        };
        Self::span(f0, f1, coarse_per.max(1e-9), self.max.len(), v)
    }

    fn span(f0: f64, f1: f64, per: f64, n: usize, v: impl Fn(usize) -> (f32, f32)) -> (f32, f32) {
        let (b0, b1) = (f0 / per, f1 / per);
        if b1 - b0 < 1.0 {
            // Within one bucket: between the two nearest bucket centres.
            let c = ((b0 + b1) / 2.0 - 0.5).max(0.0);
            let i = (c.floor() as usize).min(n - 1);
            let t = (c - i as f64).clamp(0.0, 1.0) as f32;
            let (a, b) = (v(i), v(i + 1));
            return (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        }
        let i0 = (b0.floor().max(0.0) as usize).min(n - 1);
        let i1 = (b1.ceil() as usize).clamp(i0 + 1, n);
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for i in i0..i1 {
            let (l, h) = v(i);
            lo = lo.min(l);
            hi = hi.max(h);
        }
        (lo, hi)
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
    fine: Vec<(i16, i16)>,
    fine_per: u64,
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
        let fine_per = FINE_FRAMES.max(n.div_ceil(MAX_FINE));
        OverviewBuilder {
            min: vec![f32::MAX; buckets],
            max: vec![f32::MIN; buckets],
            per: n as f64 / buckets as f64,
            fine: vec![(i16::MAX, i16::MIN); n.div_ceil(fine_per) as usize],
            fine_per,
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
            if let Some(f) = self.fine.get_mut((self.pos / self.fine_per) as usize) {
                let q = |x: f32| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                let (flo, fhi) = frame
                    .iter()
                    .fold((f.0, f.1), |(a, b), &s| (a.min(q(s)), b.max(q(s))));
                *f = (flo, fhi);
            }
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
        for f in &mut self.fine {
            if f.0 > f.1 {
                *f = (0, 0);
            }
        }
        let frames = self.pos.max(1).min(self.frames.max(self.pos));
        Overview {
            min: self.min,
            max: self.max,
            fine: self.fine,
            fine_per: self.fine_per as f64,
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
    fn a_deep_zoom_reads_the_fine_envelope() {
        // 10 s at 1 kHz: silence with one 4 ms click at 5 s. The coarse envelope's
        // buckets are wider than the click; the fine one shows it on its own.
        let mut v = vec![0.0f32; 10_000];
        for x in &mut v[5000..5004] {
            *x = 0.8;
        }
        let buf = SampleBuffer::from_interleaved(1000, 1, v);
        let p = Overview::from_buffer(&buf);
        let (_, hi) = p.envelope(5.0, 5.004);
        assert!((hi - 0.8).abs() < 0.01, "{hi}");
        // Just before the click, a few ms wide: nothing there.
        assert!(p.envelope(4.97, 4.98).1 < 0.01);
        // Wide spans still use the whole-file envelope.
        assert!((p.envelope(0.0, 10.0).1 - 0.8).abs() < 0.01);
    }

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
