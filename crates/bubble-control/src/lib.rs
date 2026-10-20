//! Bubble control layer: turns pointer, keyboard and MIDI input into engine
//! commands according to the project (bindings, cart behaviours), keeps assets
//! armed, and records a playout log. No UI-framework dependencies.

pub mod assets;
pub mod controller;
pub mod input;
pub mod memory;

pub use assets::{AssetCache, AssetState};
pub use controller::{
    AttrSet, CartClip, CartRuntime, CartState, ControlError, Controller, Conversion, Learn,
    LogEntry, LogKind, MouseButton, PageClip, ReassignScope, Route, display_label,
};
pub use input::{InputEvent, MidiEvent, TriggerSource, describe};
