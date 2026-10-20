//! Construction of a mixer and its control-side handle.

use std::sync::Arc;

use crate::buffer::SampleBuffer;
use crate::command::{EngineCommand, EngineEvent};
use crate::mixer::Mixer;
use crate::status::EngineStatus;

/// Engine sizing.
#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub sample_rate: u32,
    pub channels: u16,
    /// Maximum simultaneous voices (default 64).
    pub voices: usize,
    /// Command ring capacity.
    pub command_capacity: usize,
    /// Event ring capacity.
    pub event_capacity: usize,
    /// Ramp (ms) when a voice starts on a non-silent sample; 0 disables it.
    pub declick_ms: f32,
}

impl EngineConfig {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            voices: 64,
            command_capacity: 1024,
            event_capacity: 4096,
            declick_ms: 1.5,
        }
    }
}

/// Error returned when the command ring is full; carries the command back.
#[derive(Debug)]
pub struct CommandQueueFull(pub EngineCommand);

impl std::fmt::Display for CommandQueueFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("engine command queue is full")
    }
}

impl std::error::Error for CommandQueueFull {}

/// Control-side end of the engine: send commands, receive events, release buffers.
/// `Send` but not `Sync`: own it from one thread (or behind a mutex).
pub struct EngineHandle {
    commands: rtrb::Producer<EngineCommand>,
    events: rtrb::Consumer<EngineEvent>,
    garbage: rtrb::Consumer<Arc<SampleBuffer>>,
    status: Arc<EngineStatus>,
}

impl EngineHandle {
    /// Queues a command for the next audio block. Never blocks.
    pub fn send(&mut self, cmd: EngineCommand) -> Result<(), CommandQueueFull> {
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(c)| CommandQueueFull(c))
    }

    /// Drains pending events into `f`. Also releases retired buffers.
    pub fn poll_events(&mut self, mut f: impl FnMut(EngineEvent)) {
        while let Ok(ev) = self.events.pop() {
            f(ev);
        }
        self.collect_garbage();
    }

    /// Drops buffers retired by the audio thread (on this, non-real-time, thread).
    pub fn collect_garbage(&mut self) -> usize {
        let mut n = 0;
        while let Ok(buf) = self.garbage.pop() {
            drop(buf);
            n += 1;
        }
        n
    }

    pub fn status(&self) -> &Arc<EngineStatus> {
        &self.status
    }
}

/// Factory for a connected (`EngineHandle`, `Mixer`) pair.
pub struct Engine;

impl Engine {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(config: EngineConfig) -> (EngineHandle, Mixer) {
        let (cmd_tx, cmd_rx) = rtrb::RingBuffer::new(config.command_capacity);
        let (ev_tx, ev_rx) = rtrb::RingBuffer::new(config.event_capacity);
        let garbage_cap = config.voices * 2 + config.command_capacity;
        let (gb_tx, gb_rx) = rtrb::RingBuffer::new(garbage_cap);
        let status = Arc::new(EngineStatus::new(
            config.sample_rate,
            config.channels,
            config.voices,
        ));
        let mixer = Mixer::new(
            config.sample_rate,
            config.channels,
            config.voices,
            cmd_rx,
            ev_tx,
            gb_tx,
            status.clone(),
            config.declick_ms,
        );
        (
            EngineHandle {
                commands: cmd_tx,
                events: ev_rx,
                garbage: gb_rx,
                status,
            },
            mixer,
        )
    }
}
