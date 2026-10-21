//! Audio ready to play: held whole in memory, or a few pre-roll "heads" in memory
//! with the rest streamed from disk (decision D-37).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::buffer::SampleBuffer;

/// Most channels a voice reads per frame (7.1). Wider sources play their first 8.
pub const MAX_CHANNELS: usize = 8;

/// A stretch of a streamed file held in memory, starting at `start` (engine frames).
#[derive(Clone, Debug)]
pub struct Head {
    pub start: usize,
    pub buffer: Arc<SampleBuffer>,
}

impl Head {
    pub fn end(&self) -> usize {
        self.start + self.buffer.frames()
    }

    fn covers(&self, pos: usize) -> bool {
        pos >= self.start && pos < self.end()
    }
}

#[derive(Debug)]
enum Data {
    Resident(SampleBuffer),
    Streamed { path: PathBuf, heads: Box<[Head]> },
}

/// Audio at the engine sample rate, shared with voices through `Arc`. Created off
/// the audio thread (see [`crate::loader::prepare`]).
#[derive(Debug)]
pub struct Playable {
    sample_rate: u32,
    channels: u16,
    frames: usize,
    data: Data,
}

impl Playable {
    /// Audio held whole in memory.
    pub fn resident(buffer: SampleBuffer) -> Arc<Self> {
        Arc::new(Playable {
            sample_rate: buffer.sample_rate(),
            channels: buffer.channels(),
            frames: buffer.frames(),
            data: Data::Resident(buffer),
        })
    }

    /// Audio streamed from `path` (decoded to `channels` channels at `sample_rate`,
    /// `frames` long), with `heads` held in memory so a start there is instant.
    pub fn streamed(
        path: &Path,
        sample_rate: u32,
        channels: u16,
        frames: usize,
        mut heads: Vec<Head>,
    ) -> Arc<Self> {
        heads.sort_by_key(|h| h.start);
        Arc::new(Playable {
            sample_rate,
            channels,
            frames,
            data: Data::Streamed {
                path: path.to_path_buf(),
                heads: heads.into_boxed_slice(),
            },
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Length in engine frames.
    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }

    pub fn is_streamed(&self) -> bool {
        matches!(self.data, Data::Streamed { .. })
    }

    /// The whole audio, when it is held in memory.
    pub fn resident_buffer(&self) -> Option<&SampleBuffer> {
        match &self.data {
            Data::Resident(b) => Some(b),
            Data::Streamed { .. } => None,
        }
    }

    /// The file a streamed source plays from.
    pub fn path(&self) -> Option<&Path> {
        match &self.data {
            Data::Resident(_) => None,
            Data::Streamed { path, .. } => Some(path),
        }
    }

    /// Pre-roll heads of a streamed source (empty when resident).
    pub fn heads(&self) -> &[Head] {
        match &self.data {
            Data::Resident(_) => &[],
            Data::Streamed { heads, .. } => heads,
        }
    }

    /// Memory held by the samples.
    pub fn bytes(&self) -> u64 {
        match &self.data {
            Data::Resident(b) => b.bytes(),
            Data::Streamed { heads, .. } => heads.iter().map(|h| h.buffer.bytes()).sum(),
        }
    }

    /// Whether a start at `pos` plays from memory without waiting for the disk.
    pub fn in_memory(&self, pos: usize) -> bool {
        match &self.data {
            Data::Resident(b) => pos < b.frames(),
            Data::Streamed { heads, .. } => heads.iter().any(|h| h.covers(pos)),
        }
    }

    /// Where a voice at `pos` needs the disk to take over: the end of the head that
    /// covers `pos`, or `pos` itself.
    pub fn stream_from(&self, pos: usize) -> usize {
        self.heads()
            .iter()
            .filter(|h| h.covers(pos))
            .map(Head::end)
            .max()
            .unwrap_or(pos)
    }

    /// Copies frame `pos` into `out` when it is in memory. Real-time safe.
    #[inline]
    pub(crate) fn memory_frame(&self, pos: usize, out: &mut [f32; MAX_CHANNELS]) -> bool {
        let (buf, at) = match &self.data {
            Data::Resident(b) => (b, pos),
            Data::Streamed { heads, .. } => match heads.iter().find(|h| h.covers(pos)) {
                Some(h) => (&*h.buffer, pos - h.start),
                None => return false,
            },
        };
        if at >= buf.frames() {
            return false;
        }
        let f = buf.frame(at);
        for (c, o) in out.iter_mut().enumerate().take(f.len()) {
            *o = f.get(c);
        }
        true
    }
}
