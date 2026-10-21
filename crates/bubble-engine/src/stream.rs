//! Disk streaming for long files (decision D-37).
//!
//! Each playing voice of a streamed [`Playable`] owns a [`VoiceStream`]: the reading
//! end of a lock-free ring (`rtrb`) that one disk-reader thread keeps several seconds
//! ahead, decoding and resampling as it goes. The voice starts from a pre-roll head in
//! memory, so a trigger never waits for the disk; the ring takes over where the head
//! ends.
//!
//! The voice asks for data from a new place (loop wrap, seek, trim change) with a
//! *request*: a position tagged with an epoch, written to an atomic. The reader
//! answers with an *ack* carrying how many samples it had written before switching,
//! so the voice skips exactly the stale samples still in the ring. Nothing on the
//! voice side blocks, allocates or frees: when data is not there yet the voice gets
//! [`Fetch::Wait`] and plays silence.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::decode::StreamDecoder;
use crate::playable::{MAX_CHANNELS, Playable};

/// How far ahead of the voice the reader decodes.
pub const RING_SECS: f64 = 4.0;
/// Least free space (frames) worth a decode pass on one stream.
const MIN_FILL: usize = 1024;
/// Most frames decoded for one stream before turning to the next.
const MAX_FILL: usize = 8192;
/// How often an idle reader looks for requests.
const POLL: Duration = Duration::from_millis(4);

const POS_BITS: u32 = 48;
const POS_MASK: u64 = (1 << POS_BITS) - 1;

fn pack(epoch: u16, value: u64) -> u64 {
    (epoch as u64) << POS_BITS | (value & POS_MASK)
}

fn unpack(v: u64) -> (u16, u64) {
    ((v >> POS_BITS) as u16, v & POS_MASK)
}

/// State shared by a voice and the reader.
struct Shared {
    /// Latest request from the voice: (epoch, engine frame).
    request: AtomicU64,
    /// The reader switched to an epoch: (epoch, samples written before the switch).
    ack: AtomicU64,
    /// The reader ran out of file for an epoch: (epoch, samples written in all).
    eof: AtomicU64,
}

/// Result of asking a stream for a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fetch {
    Ok,
    /// Not there yet. `starved` when the reader is behind (an underrun), as opposed to
    /// still answering a request for a new place.
    Wait {
        starved: bool,
    },
    /// The file ends before this frame.
    End,
}

/// The voice's end of a stream. Created by [`Streamer::open`] off the audio thread
/// and moved into the mixer with the Play command.
pub struct VoiceStream {
    ring: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    ch: usize,
    epoch: u16,
    /// Position of the latest request.
    req_pos: usize,
    /// Waiting for the reader to acknowledge the latest request.
    pending: bool,
    /// The latest request was made ahead of the voice (where a head ends), so having to
    /// wait for it is an underrun rather than the cost of a jump.
    ahead: bool,
    /// Engine frame of the next frame in the ring (once synced).
    ring_pos: usize,
    /// Samples taken from the ring in all.
    read: u64,
    cap_frames: usize,
    /// The last two frames taken from the ring, for interpolation.
    last: [f32; MAX_CHANNELS],
    last_pos: usize,
    prev: [f32; MAX_CHANNELS],
    prev_pos: usize,
}

// SAFETY: the ring consumer (not `Sync`) is only touched through `&mut self`; a shared
// `&VoiceStream` reaches nothing but plain fields. This lets a command carrying a
// stream be an error type (`CommandQueueFull`).
unsafe impl Sync for VoiceStream {}

impl std::fmt::Debug for VoiceStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceStream")
            .field("channels", &self.ch)
            .field("req_pos", &self.req_pos)
            .field("pending", &self.pending)
            .field("ring_pos", &self.ring_pos)
            .finish()
    }
}

impl VoiceStream {
    /// Asks the reader for data from `pos` on, unless the ring already continues there.
    /// `ahead` when the voice is still elsewhere (in a head ending at `pos`).
    pub(crate) fn request(&mut self, pos: usize, ahead: bool) {
        if self.pending && self.req_pos == pos {
            return;
        }
        if !self.pending && self.ring_pos == pos {
            return;
        }
        self.epoch = match self.epoch.wrapping_add(1) {
            0 => 1,
            e => e,
        };
        self.req_pos = pos;
        self.pending = true;
        self.ahead = ahead;
        self.shared
            .request
            .store(pack(self.epoch, pos as u64), Ordering::Release);
    }

    /// Takes the reader's answer to the latest request, dropping stale samples.
    fn sync(&mut self) {
        if !self.pending {
            return;
        }
        let (epoch, written) = unpack(self.shared.ack.load(Ordering::Acquire));
        if epoch != self.epoch {
            return;
        }
        let stale = written.saturating_sub(self.read) as usize;
        let n = stale.min(self.ring.slots());
        if let Ok(chunk) = self.ring.read_chunk(n) {
            chunk.commit_all();
        }
        self.read += n as u64;
        if n < stale {
            // Cannot happen (the stale samples were pushed before the ack); try again.
            return;
        }
        self.pending = false;
        self.ring_pos = self.req_pos;
    }

    /// Whether the reader has written all there is for the current request.
    fn eof_reached(&self) -> bool {
        let (epoch, _) = unpack(self.shared.eof.load(Ordering::Acquire));
        epoch == self.epoch
    }

    fn skip(&mut self, frames: usize) {
        let n = frames * self.ch;
        if let Ok(chunk) = self.ring.read_chunk(n) {
            chunk.commit_all();
        }
        self.read += n as u64;
        self.ring_pos += frames;
    }

    /// Copies frame `pos` into `out`. With `may_request`, a frame the ring cannot reach
    /// is asked for; without (a look-ahead for interpolation) nothing changes but the
    /// ring moving forward. Real-time safe.
    pub(crate) fn fetch(
        &mut self,
        pos: usize,
        out: &mut [f32; MAX_CHANNELS],
        may_request: bool,
    ) -> Fetch {
        let ch = self.ch;
        if pos == self.last_pos {
            out[..ch].copy_from_slice(&self.last[..ch]);
            return Fetch::Ok;
        }
        if pos == self.prev_pos {
            out[..ch].copy_from_slice(&self.prev[..ch]);
            return Fetch::Ok;
        }
        self.sync();
        if self.pending {
            if may_request && self.req_pos > pos {
                self.request(pos, false);
            }
            return Fetch::Wait {
                starved: self.ahead && pos >= self.req_pos,
            };
        }
        if pos < self.ring_pos || pos - self.ring_pos > self.cap_frames {
            if may_request {
                self.request(pos, false);
            }
            return Fetch::Wait { starved: false };
        }
        // Read the end-of-file mark before the ring, so data pushed before it is seen.
        let eof = self.eof_reached();
        let skip = pos - self.ring_pos;
        let avail = self.ring.slots() / ch;
        if avail <= skip {
            self.skip(avail);
            return if eof {
                Fetch::End
            } else {
                Fetch::Wait { starved: true }
            };
        }
        self.skip(skip);
        let Ok(chunk) = self.ring.read_chunk(ch) else {
            return Fetch::Wait { starved: true };
        };
        self.prev = self.last;
        self.prev_pos = self.last_pos;
        let (a, b) = chunk.as_slices();
        self.last[..a.len()].copy_from_slice(a);
        self.last[a.len()..ch].copy_from_slice(b);
        chunk.commit_all();
        self.read += ch as u64;
        self.last_pos = pos;
        self.ring_pos = pos + 1;
        out[..ch].copy_from_slice(&self.last[..ch]);
        Fetch::Ok
    }
}

/// The reader's end of a stream.
struct ReaderStream {
    ring: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    path: PathBuf,
    rate: u32,
    ch: usize,
    decoder: Option<StreamDecoder>,
    epoch: u16,
    written: u64,
    done: bool,
    scratch: Vec<f32>,
}

impl ReaderStream {
    fn fill_level(&self) -> f64 {
        let cap = self.ring.buffer().capacity();
        1.0 - self.ring.slots() as f64 / cap as f64
    }

    fn finish(&mut self) {
        self.done = true;
        self.shared
            .eof
            .store(pack(self.epoch, self.written), Ordering::Release);
    }

    /// One step of work. Returns whether anything was done.
    fn step(&mut self) -> bool {
        let (epoch, pos) = unpack(self.shared.request.load(Ordering::Acquire));
        if epoch == 0 {
            return false;
        }
        if epoch != self.epoch {
            self.epoch = epoch;
            self.done = false;
            let ok = (|| {
                if self.decoder.is_none() {
                    self.decoder = Some(StreamDecoder::open(&self.path, self.rate)?);
                }
                let d = self.decoder.as_mut().expect("decoder");
                if d.channels() as usize != self.ch {
                    return Err(crate::loader::LoadError::Decode(
                        "file changed since it was loaded".into(),
                    ));
                }
                d.seek(pos)
            })();
            self.shared
                .ack
                .store(pack(epoch, self.written), Ordering::Release);
            if ok.is_err() {
                self.decoder = None;
                self.finish();
            }
            return true;
        }
        if self.done {
            return false;
        }
        let free = self.ring.slots() / self.ch;
        if free < MIN_FILL {
            return false;
        }
        let want = free.min(MAX_FILL);
        self.scratch.clear();
        let got = match self
            .decoder
            .as_mut()
            .map(|d| d.read(&mut self.scratch, want))
        {
            Some(Ok(n)) => n,
            _ => 0,
        };
        if got > 0 {
            // Fits: `want` frames were free and only this thread writes.
            let _ = self.ring.push_entire_slice(&self.scratch[..got * self.ch]);
            self.written += (got * self.ch) as u64;
        }
        if got < want {
            self.finish();
        }
        true
    }
}

/// Counters of the reader thread, for diagnostics and tests.
#[derive(Default)]
struct ReaderStats {
    /// Passes in which there was nothing to do.
    idle_passes: AtomicU64,
    /// Streams handed to the reader, and taken up by it.
    sent: AtomicU64,
    received: AtomicU64,
    /// The reader has no streams and waits for one.
    empty: AtomicBool,
    paused: AtomicBool,
}

/// The disk-reader thread. One serves every voice of every engine; it ends when the
/// `Streamer` is dropped.
pub struct Streamer {
    tx: Sender<ReaderStream>,
    stats: Arc<ReaderStats>,
}

impl Streamer {
    pub fn new() -> Self {
        let (tx, rx) = channel::<ReaderStream>();
        let stats = Arc::new(ReaderStats::default());
        let st = stats.clone();
        thread::Builder::new()
            .name("bubble-stream".into())
            .spawn(move || reader_loop(rx, st))
            .expect("spawn stream reader");
        Streamer { tx, stats }
    }

    /// A stream for a voice of `audio` that starts at engine frame `start`; `None`
    /// when `audio` is held in memory. The reader starts filling at once, from where
    /// the head covering `start` ends.
    pub fn open(&self, audio: &Arc<Playable>, start: usize) -> Option<Box<VoiceStream>> {
        let path = audio.path()?.to_path_buf();
        let ch = audio.channels() as usize;
        if ch == 0 || ch > MAX_CHANNELS {
            return None;
        }
        let cap_frames = (RING_SECS * audio.sample_rate() as f64) as usize;
        let (producer, consumer) = rtrb::RingBuffer::new(cap_frames * ch);
        let from = audio.stream_from(start);
        let shared = Arc::new(Shared {
            request: AtomicU64::new(pack(1, from as u64)),
            ack: AtomicU64::new(0),
            eof: AtomicU64::new(0),
        });
        let reader = ReaderStream {
            ring: producer,
            shared: shared.clone(),
            path,
            rate: audio.sample_rate(),
            ch,
            decoder: None,
            epoch: 0,
            written: 0,
            done: false,
            scratch: Vec::with_capacity(MAX_FILL * ch),
        };
        self.stats.sent.fetch_add(1, Ordering::AcqRel);
        self.tx.send(reader).ok()?;
        Some(Box::new(VoiceStream {
            ring: consumer,
            shared,
            ch,
            epoch: 1,
            req_pos: from,
            pending: true,
            ahead: from > start,
            ring_pos: 0,
            read: 0,
            cap_frames,
            last: [0.0; MAX_CHANNELS],
            last_pos: usize::MAX,
            prev: [0.0; MAX_CHANNELS],
            prev_pos: usize::MAX,
        }))
    }

    /// Stops (or resumes) reading, to test underruns.
    #[doc(hidden)]
    pub fn set_paused(&self, paused: bool) {
        self.stats.paused.store(paused, Ordering::Relaxed);
    }

    /// Waits until the reader has answered every request and filled every ring it
    /// can (for tests). Returns false on timeout.
    #[doc(hidden)]
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        let n = self.stats.idle_passes.load(Ordering::Acquire);
        while start.elapsed() < timeout {
            let st = &self.stats;
            if st.idle_passes.load(Ordering::Acquire) >= n + 2
                || st.empty.load(Ordering::Acquire)
                    && st.received.load(Ordering::Acquire) == st.sent.load(Ordering::Acquire)
            {
                return true;
            }
            thread::sleep(Duration::from_micros(200));
        }
        false
    }
}

impl Default for Streamer {
    fn default() -> Self {
        Self::new()
    }
}

fn reader_loop(rx: Receiver<ReaderStream>, stats: Arc<ReaderStats>) {
    let mut streams: Vec<ReaderStream> = Vec::new();
    let mut idle = false;
    loop {
        // New streams: wait for one when there is nothing else to do.
        let first = if streams.is_empty() {
            stats.empty.store(true, Ordering::Release);
            let s = rx.recv();
            stats.empty.store(false, Ordering::Release);
            match s {
                Ok(s) => Some(s),
                Err(_) => return,
            }
        } else if idle {
            match rx.recv_timeout(POLL) {
                Ok(s) => Some(s),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            None
        };
        let before = streams.len();
        streams.extend(first);
        streams.extend(rx.try_iter());
        stats
            .received
            .fetch_add((streams.len() - before) as u64, Ordering::AcqRel);
        streams.retain(|s| !s.ring.is_abandoned());
        if stats.paused.load(Ordering::Relaxed) {
            idle = true;
            if streams.is_empty() {
                thread::sleep(POLL);
            }
            continue;
        }
        // Emptiest ring first.
        streams.sort_by(|a, b| a.fill_level().total_cmp(&b.fill_level()));
        let mut worked = false;
        for s in &mut streams {
            worked |= s.step();
        }
        idle = !worked;
        if idle {
            stats.idle_passes.fetch_add(1, Ordering::Release);
        }
    }
}
