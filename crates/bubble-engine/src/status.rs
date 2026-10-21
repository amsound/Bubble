//! Lock-free, read-mostly view of the engine for the UI and diagnostics.
//!
//! The audio thread writes plain atomics (relaxed ordering; values are advisory
//! display data). Readers never block the audio thread.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use crate::command::CartKey;

pub const NO_CART: u32 = u32::MAX;

/// Display state of one voice slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoicePhase {
    Free,
    Playing,
    FadingIn,
    Stopping,
}

impl VoicePhase {
    fn to_u32(self) -> u32 {
        match self {
            VoicePhase::Free => 0,
            VoicePhase::Playing => 1,
            VoicePhase::FadingIn => 2,
            VoicePhase::Stopping => 3,
        }
    }
    fn from_u32(v: u32) -> Self {
        match v {
            1 => VoicePhase::Playing,
            2 => VoicePhase::FadingIn,
            3 => VoicePhase::Stopping,
            _ => VoicePhase::Free,
        }
    }
}

#[derive(Default)]
pub(crate) struct VoiceSlotStatus {
    cart: AtomicU32,
    phase: AtomicU32,
    /// Frames played since the In point (wraps on loop).
    position: AtomicU64,
    /// Frames between In and Out.
    length: AtomicU64,
    looping: AtomicBool,
    /// Output peaks since the UI last took them (f32 bits; fetch_max works for ≥ 0).
    peak_l: AtomicU32,
    peak_r: AtomicU32,
    /// Current playback rate (f32 bits).
    speed: AtomicU32,
}

/// Snapshot of one active voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceView {
    pub cart: CartKey,
    pub phase: VoicePhase,
    pub position_frames: u64,
    pub length_frames: u64,
    pub looping: bool,
    /// Current playback speed (1 = normal).
    pub speed: f32,
}

/// Shared engine status. Create through [`crate::Engine::new`].
pub struct EngineStatus {
    sample_rate: u32,
    channels: u16,
    pub(crate) voices: Box<[VoiceSlotStatus]>,
    pub(crate) callbacks: AtomicU64,
    pub(crate) frames_rendered: AtomicU64,
    pub(crate) last_callback_frames: AtomicU32,
    pub(crate) max_callback_frames: AtomicU32,
    pub(crate) min_callback_frames: AtomicU32,
    pub(crate) active_voices: AtomicU32,
    /// Output peak since last read, as f32 bits, per first two channels.
    pub(crate) peak_l: AtomicU32,
    pub(crate) peak_r: AtomicU32,
    pub(crate) limiter_active_blocks: AtomicU64,
    pub(crate) events_dropped: AtomicU64,
    pub(crate) garbage_overflow: AtomicU64,
    pub(crate) voices_stolen: AtomicU64,
    /// Device-reported time between callback and playback of the first frame (µs).
    pub(crate) output_latency_us: AtomicU64,
    /// Longest callback processing time observed (ns), measured by the device layer.
    pub(crate) max_process_ns: AtomicU64,
    pub(crate) xruns: AtomicU64,
    /// Times a streamed voice ran out of data mid-play (the disk reader fell behind).
    pub(crate) stream_underruns: AtomicU64,
    /// Frames output silent because of those underruns.
    pub(crate) stream_underrun_frames: AtomicU64,
    /// Frames a voice waited for the disk after a jump outside the pre-roll heads
    /// (a seek or trim change), not counted as underruns.
    pub(crate) stream_wait_frames: AtomicU64,
}

impl EngineStatus {
    pub(crate) fn new(sample_rate: u32, channels: u16, voices: usize) -> Self {
        let slots = (0..voices)
            .map(|_| {
                let s = VoiceSlotStatus::default();
                s.cart.store(NO_CART, Relaxed);
                s
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            sample_rate,
            channels,
            voices: slots,
            callbacks: AtomicU64::new(0),
            frames_rendered: AtomicU64::new(0),
            last_callback_frames: AtomicU32::new(0),
            max_callback_frames: AtomicU32::new(0),
            min_callback_frames: AtomicU32::new(u32::MAX),
            active_voices: AtomicU32::new(0),
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
            limiter_active_blocks: AtomicU64::new(0),
            events_dropped: AtomicU64::new(0),
            garbage_overflow: AtomicU64::new(0),
            voices_stolen: AtomicU64::new(0),
            output_latency_us: AtomicU64::new(0),
            max_process_ns: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            stream_underruns: AtomicU64::new(0),
            stream_underrun_frames: AtomicU64::new(0),
            stream_wait_frames: AtomicU64::new(0),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn voice_capacity(&self) -> usize {
        self.voices.len()
    }

    pub(crate) fn publish_voice(
        &self,
        slot: usize,
        cart: CartKey,
        phase: VoicePhase,
        pos: u64,
        len: u64,
        looping: bool,
    ) {
        let s = &self.voices[slot];
        s.cart.store(cart.0, Relaxed);
        s.phase.store(phase.to_u32(), Relaxed);
        s.position.store(pos, Relaxed);
        s.length.store(len, Relaxed);
        s.looping.store(looping, Relaxed);
    }

    pub(crate) fn publish_voice_meter(&self, slot: usize, l: f32, r: f32, speed: f32) {
        let s = &self.voices[slot];
        s.peak_l.fetch_max(l.max(0.0).to_bits(), Relaxed);
        s.peak_r.fetch_max(r.max(0.0).to_bits(), Relaxed);
        s.speed.store(speed.to_bits(), Relaxed);
    }

    /// Returns and resets each active voice's output peaks (linear, after gain and pan),
    /// for per-cart level meters. Allocation-free.
    pub fn take_voice_peaks(&self, mut f: impl FnMut(CartKey, f32, f32)) {
        for s in self.voices.iter() {
            let cart = s.cart.load(Relaxed);
            let l = f32::from_bits(s.peak_l.swap(0, Relaxed));
            let r = f32::from_bits(s.peak_r.swap(0, Relaxed));
            if cart != NO_CART {
                f(CartKey(cart), l, r);
            }
        }
    }

    pub(crate) fn clear_voice(&self, slot: usize) {
        let s = &self.voices[slot];
        s.peak_l.store(0, Relaxed);
        s.peak_r.store(0, Relaxed);
        s.phase.store(0, Relaxed);
        s.cart.store(NO_CART, Relaxed);
    }

    /// Calls `f` for each currently active voice. Allocation-free.
    pub fn for_each_voice(&self, mut f: impl FnMut(VoiceView)) {
        for s in self.voices.iter() {
            let cart = s.cart.load(Relaxed);
            let phase = VoicePhase::from_u32(s.phase.load(Relaxed));
            if cart == NO_CART || phase == VoicePhase::Free {
                continue;
            }
            f(VoiceView {
                cart: CartKey(cart),
                phase,
                position_frames: s.position.load(Relaxed),
                length_frames: s.length.load(Relaxed),
                looping: s.looping.load(Relaxed),
                speed: f32::from_bits(s.speed.load(Relaxed)),
            });
        }
    }

    pub fn active_voices(&self) -> u32 {
        self.active_voices.load(Relaxed)
    }

    /// Returns and resets the output peaks (linear) of channels 0 and 1.
    pub fn take_peaks(&self) -> (f32, f32) {
        let l = f32::from_bits(self.peak_l.swap(0, Relaxed));
        let r = f32::from_bits(self.peak_r.swap(0, Relaxed));
        (l, r)
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let min = self.min_callback_frames.load(Relaxed);
        Diagnostics {
            sample_rate: self.sample_rate,
            channels: self.channels,
            callbacks: self.callbacks.load(Relaxed),
            frames_rendered: self.frames_rendered.load(Relaxed),
            last_callback_frames: self.last_callback_frames.load(Relaxed),
            min_callback_frames: if min == u32::MAX { 0 } else { min },
            max_callback_frames: self.max_callback_frames.load(Relaxed),
            active_voices: self.active_voices.load(Relaxed),
            limiter_active_blocks: self.limiter_active_blocks.load(Relaxed),
            events_dropped: self.events_dropped.load(Relaxed),
            garbage_overflow: self.garbage_overflow.load(Relaxed),
            voices_stolen: self.voices_stolen.load(Relaxed),
            output_latency_us: self.output_latency_us.load(Relaxed),
            max_process_ns: self.max_process_ns.load(Relaxed),
            xruns: self.xruns.load(Relaxed),
            stream_underruns: self.stream_underruns.load(Relaxed),
            stream_underrun_frames: self.stream_underrun_frames.load(Relaxed),
            stream_wait_frames: self.stream_wait_frames.load(Relaxed),
        }
    }
}

/// Point-in-time engine counters for display and logging.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub sample_rate: u32,
    pub channels: u16,
    pub callbacks: u64,
    pub frames_rendered: u64,
    pub last_callback_frames: u32,
    pub min_callback_frames: u32,
    pub max_callback_frames: u32,
    pub active_voices: u32,
    pub limiter_active_blocks: u64,
    pub events_dropped: u64,
    pub garbage_overflow: u64,
    pub voices_stolen: u64,
    pub output_latency_us: u64,
    pub max_process_ns: u64,
    pub xruns: u64,
    pub stream_underruns: u64,
    pub stream_underrun_frames: u64,
    pub stream_wait_frames: u64,
}
