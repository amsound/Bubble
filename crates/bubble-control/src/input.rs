//! Input events from every source, in one representation.

use bubble_project::{BindingSource, MidiChannel, MidiMessage};

/// Where a trigger came from (logged with each playout, like the reference's trigger codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerSource {
    Mouse,
    Key,
    Midi,
    PlayNext,
    Ui,
}

/// A normalised MIDI message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiEvent {
    /// 1–16.
    pub channel: u8,
    pub message: MidiMessage,
    pub number: u8,
    /// Velocity or CC value.
    pub value: u8,
}

impl MidiEvent {
    /// Parses a raw short message. Note-on with velocity 0 becomes note-off.
    pub fn parse(bytes: &[u8]) -> Option<MidiEvent> {
        let status = *bytes.first()?;
        if !(0x80..0xF0).contains(&status) {
            return None;
        }
        let channel = (status & 0x0F) + 1;
        let d1 = bytes.get(1).copied().unwrap_or(0) & 0x7F;
        let d2 = bytes.get(2).copied().unwrap_or(0) & 0x7F;
        let (message, value) = match status & 0xF0 {
            0x90 if d2 == 0 => (MidiMessage::NoteOff, 0),
            0x90 => (MidiMessage::NoteOn, d2),
            0x80 => (MidiMessage::NoteOff, d2),
            0xB0 => (MidiMessage::Cc, d2),
            0xC0 => (MidiMessage::Program, 0),
            _ => return None,
        };
        Some(MidiEvent {
            channel,
            message,
            number: d1,
            value,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        /// Physical key code name ("F1", "KeyA", "Numpad5", "Escape", …).
        code: String,
        /// Sorted modifier names ("Alt", "Ctrl", "Shift", "Meta").
        modifiers: Vec<String>,
        pressed: bool,
        repeat: bool,
    },
    Midi {
        port: String,
        event: MidiEvent,
    },
}

/// How a binding source relates to an input event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The trigger edge (key down, note on, CC ≥ threshold).
    Press,
    /// The release edge of the same control (key up, note off) — used by hold-to-play.
    Release,
    No,
}

pub fn matches(source: &BindingSource, ev: &InputEvent) -> Hit {
    match (source, ev) {
        (
            BindingSource::Key { key, modifiers },
            InputEvent::Key {
                code,
                modifiers: m,
                pressed,
                repeat,
            },
        ) => {
            if key != code || *repeat {
                return Hit::No;
            }
            if *pressed {
                let mut want = modifiers.clone();
                want.sort();
                if &want == m { Hit::Press } else { Hit::No }
            } else {
                Hit::Release
            }
        }
        (
            BindingSource::Midi {
                device,
                channel,
                message,
                number,
                threshold,
            },
            InputEvent::Midi { port, event },
        ) => {
            if device != "*" && device != port {
                return Hit::No;
            }
            if let MidiChannel(Some(c)) = channel
                && *c != event.channel
            {
                return Hit::No;
            }
            if *number != event.number {
                return Hit::No;
            }
            match (message, event.message) {
                (MidiMessage::NoteOn, MidiMessage::NoteOn) => Hit::Press,
                (MidiMessage::NoteOn, MidiMessage::NoteOff) => Hit::Release,
                (MidiMessage::NoteOff, MidiMessage::NoteOff) => Hit::Press,
                (MidiMessage::Program, MidiMessage::Program) => Hit::Press,
                (MidiMessage::Cc, MidiMessage::Cc) => {
                    let t = threshold.unwrap_or(64);
                    if event.value >= t {
                        Hit::Press
                    } else {
                        Hit::Release
                    }
                }
                _ => Hit::No,
            }
        }
        _ => Hit::No,
    }
}

/// A binding source that would match this event (used by "learn" mode).
pub fn source_from_event(ev: &InputEvent) -> Option<BindingSource> {
    match ev {
        InputEvent::Key {
            code,
            modifiers,
            pressed: true,
            repeat: false,
        } => Some(BindingSource::Key {
            key: code.clone(),
            modifiers: modifiers.clone(),
        }),
        InputEvent::Midi { event, .. } => {
            let message = match event.message {
                MidiMessage::NoteOff => return None,
                m => m,
            };
            Some(BindingSource::Midi {
                device: "*".into(),
                channel: MidiChannel(Some(event.channel)),
                message,
                number: event.number,
                threshold: (message == MidiMessage::Cc).then_some(64),
            })
        }
        _ => None,
    }
}

/// Short human-readable label for a binding source ("F1", "Ctrl+KeyA", "N60 ch1").
pub fn describe(source: &BindingSource) -> String {
    match source {
        BindingSource::Key { key, modifiers } => {
            let k = key
                .strip_prefix("Key")
                .or_else(|| key.strip_prefix("Digit"))
                .unwrap_or(key);
            if modifiers.is_empty() {
                k.to_string()
            } else {
                format!("{}+{k}", modifiers.join("+"))
            }
        }
        BindingSource::Midi {
            channel,
            message,
            number,
            ..
        } => {
            let ch = channel.0.map_or("*".to_string(), |c| c.to_string());
            let m = match message {
                MidiMessage::NoteOn | MidiMessage::NoteOff => "N",
                MidiMessage::Cc => "CC",
                MidiMessage::Program => "PC",
            };
            format!("{m}{number} ch{ch}")
        }
    }
}
