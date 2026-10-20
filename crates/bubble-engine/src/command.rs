//! Messages between the control side and the real-time mixer.
//!
//! Commands travel through a wait-free SPSC ring into the audio callback and are
//! applied at the start of the next processing block. Everything here is plain
//! data: no allocation happens when a command is consumed.

use std::sync::Arc;

use crate::buffer::SampleBuffer;

/// Identifies a cart (or any other playable slot) to the engine. The control
/// layer maps its own stable ids onto these small integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CartKey(pub u32);

/// Unique id of one playing instance (a cart may have several when overlapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId(pub u64);

/// Shape of a gain transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FadeCurve {
    Linear,
    /// Sine/cosine quarter-wave (constant power for crossfades). Default.
    #[default]
    EqualPower,
    /// Linear in decibels from −60 dB.
    Exponential,
}

/// A fade of a given length in frames at the engine sample rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Fade {
    pub frames: u32,
    pub curve: FadeCurve,
}

impl Fade {
    pub const NONE: Fade = Fade {
        frames: 0,
        curve: FadeCurve::EqualPower,
    };

    pub fn from_ms(ms: f32, sample_rate: u32, curve: FadeCurve) -> Self {
        let frames = (ms.max(0.0) * sample_rate as f32 / 1000.0).round() as u32;
        Fade { frames, curve }
    }
}

/// What to do when a cart is triggered while it already has active voices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Retrigger {
    /// Stop existing voices with a short anti-click ramp, start again from the In point.
    #[default]
    Restart,
    /// Start an additional voice; existing ones continue.
    Overlap,
    /// If playing, stop (with the cart's stop fade); otherwise play.
    ToggleStop,
    /// Ignore the trigger while playing.
    Ignore,
}

/// Which other voices to stop when this cart starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StopOthers {
    #[default]
    None,
    /// Every other playing voice.
    All,
    /// Other voices tagged with the same page.
    Page,
}

/// Everything the mixer needs to start a cart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayParams {
    /// Linear gain (already includes mute and dB conversion).
    pub gain: f32,
    /// −1.0 (left) … +1.0 (right).
    pub pan: f32,
    /// First frame to play (In point), in buffer frames.
    pub start_frame: usize,
    /// One past the last frame (Out point); `None` = end of buffer.
    pub end_frame: Option<usize>,
    pub looping: bool,
    pub fade_in: Fade,
    /// Fade used when the voice is stopped without an explicit fade.
    pub fade_out: Fade,
    pub retrigger: Retrigger,
    /// Exclusive group (0 = none). Starting a member stops other carts in the group.
    pub group: u32,
    /// Page tag for page-scoped stops.
    pub page: u32,
    pub stop_others: StopOthers,
    /// Whether `stop_others` uses the other voices' fade-outs (true) or a fast ramp.
    pub stop_others_fade: bool,
    /// Playback speed (1 = normal, 0…4).
    pub speed: f32,
    /// First device channel of this cart's output (0-based).
    pub output_offset: u16,
    /// Channels of this cart's output: 1 (mono: L+R summed), 2 (stereo) or 6 (5.1: a
    /// 6-channel source is routed channel for channel, anything else to L/R).
    pub output_channels: u16,
}

impl Default for PlayParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            start_frame: 0,
            end_frame: None,
            looping: false,
            fade_in: Fade::NONE,
            fade_out: Fade::NONE,
            retrigger: Retrigger::Restart,
            group: 0,
            page: 0,
            stop_others: StopOthers::None,
            stop_others_fade: true,
            speed: 1.0,
            output_offset: 0,
            output_channels: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopTarget {
    Cart(CartKey),
    Page(u32),
    /// Every voice in an exclusive group (non-zero id).
    Group(u32),
    All,
}

/// Command to the real-time mixer.
#[derive(Clone, Debug)]
pub enum EngineCommand {
    Play {
        cart: CartKey,
        buffer: Arc<SampleBuffer>,
        params: PlayParams,
    },
    /// Stop with the given fade, or each voice's own fade-out when `None`.
    Stop {
        target: StopTarget,
        fade: Option<Fade>,
    },
    /// Immediate stop with a ~2 ms anti-click ramp (panic).
    Kill { target: StopTarget },
    /// Change gain of a cart's playing voices (ramped).
    SetGain { cart: CartKey, gain: f32 },
    /// Change pan of a cart's playing voices (ramped).
    SetPan { cart: CartKey, pan: f32 },
    /// Change playback speed of a cart's voices, ramping over `ramp_frames`
    /// (e.g. a record-player slow-down to 0 and spin-up back to 1).
    SetSpeed {
        cart: CartKey,
        speed: f32,
        ramp_frames: u32,
    },
    /// Turn looping on/off for playing voices (REF "Loop+").
    SetLoop { cart: CartKey, looping: bool },
    /// Master output gain (linear).
    SetMaster { gain: f32 },
    /// Enable/disable the output safety limiter.
    SetLimiter { enabled: bool },
}

/// Why a voice ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// Reached its Out point without looping.
    Completed,
    /// Stopped by a command or by another cart (group, stop-others) after its fade.
    Stopped,
    /// Replaced by a retrigger of the same cart.
    Restarted,
    /// Taken for a new voice because the pool was full.
    Stolen,
}

/// Notification from the real-time mixer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    VoiceStarted {
        cart: CartKey,
        voice: VoiceId,
    },
    VoiceEnded {
        cart: CartKey,
        voice: VoiceId,
        reason: EndReason,
    },
    /// A Play command did nothing (e.g. `Retrigger::Ignore` while playing, or empty buffer).
    PlayIgnored {
        cart: CartKey,
    },
}
