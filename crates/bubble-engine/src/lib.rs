//! Bubble audio engine.
//!
//! * [`Engine::new`] creates a connected [`EngineHandle`] (control side) and [`Mixer`]
//!   (audio side). The mixer can be driven by a real device ([`device::AudioOutput`])
//!   or rendered offline in tests.
//! * Commands ([`EngineCommand`]) and events ([`EngineEvent`]) travel through wait-free
//!   SPSC rings; display state is exposed through atomics ([`EngineStatus`]).
//! * Audio is prepared off the audio thread by [`loader`]; long files are streamed
//!   from disk by [`stream`].

pub mod buffer;
pub mod command;
mod decode;
pub mod device;
pub mod engine;
pub mod loader;
pub mod mixer;
pub mod overview;
pub mod playable;
pub mod status;
pub mod stream;

pub use buffer::SampleBuffer;
pub use command::{
    CartKey, EndReason, EngineCommand, EngineEvent, Fade, FadeCurve, PlayParams, Retrigger,
    StopOthers, StopTarget, VoiceId,
};
pub use engine::{CommandQueueFull, Engine, EngineConfig, EngineHandle};
pub use mixer::{MAX_BLOCK, Mixer};
pub use overview::Overview;
pub use playable::Playable;
pub use status::{Diagnostics, EngineStatus, VoicePhase, VoiceView};
pub use stream::{Streamer, VoiceStream};

/// Converts decibels to linear gain (−inf for very low values).
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -120.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Converts linear gain to decibels.
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * gain.log10()
    }
}
