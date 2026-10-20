//! MIDI input. Each port's callback runs on midir's thread and goes straight to the
//! controller, then to the engine command ring. The UI is never on this path.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bubble_control::{Controller, InputEvent, MidiEvent};
use midir::{Ignore, MidiInput, MidiInputConnection};

pub struct MidiInputs {
    connections: Vec<(String, MidiInputConnection<()>)>,
    /// Milliseconds since `epoch` of the last received message (0 = never).
    pub last_activity_ms: Arc<AtomicU64>,
    /// Longest time a callback waited for the controller lock (µs) — monitors decision D-06.
    pub max_lock_wait_us: Arc<AtomicU64>,
    pub epoch: Instant,
    pub errors: Vec<String>,
}

pub fn available_ports() -> Vec<String> {
    let Ok(m) = MidiInput::new("Bubble (scan)") else {
        return vec![];
    };
    m.ports()
        .iter()
        .filter_map(|p| m.port_name(p).ok())
        .collect()
}

impl MidiInputs {
    pub fn empty() -> Self {
        MidiInputs {
            connections: Vec::new(),
            last_activity_ms: Arc::new(AtomicU64::new(0)),
            max_lock_wait_us: Arc::new(AtomicU64::new(0)),
            epoch: Instant::now(),
            errors: Vec::new(),
        }
    }

    /// Opens `wanted` ports (all ports when empty).
    pub fn open(
        ctrl: &Arc<Mutex<Controller>>,
        wanted: &[String],
        repaint: impl Fn() + Send + Sync + Clone + 'static,
    ) -> Self {
        let mut me = MidiInputs::empty();
        let names = available_ports();
        for name in names {
            if !wanted.is_empty() && !wanted.contains(&name) {
                continue;
            }
            let mut input = match MidiInput::new("Bubble") {
                Ok(m) => m,
                Err(e) => {
                    me.errors.push(format!("MIDI unavailable: {e}"));
                    return me;
                }
            };
            input.ignore(Ignore::All);
            let Some(port) = input
                .ports()
                .into_iter()
                .find(|p| input.port_name(p).ok().as_deref() == Some(name.as_str()))
            else {
                continue;
            };
            let ctrl = ctrl.clone();
            let activity = me.last_activity_ms.clone();
            let lock_wait = me.max_lock_wait_us.clone();
            let epoch = me.epoch;
            let port_name = name.clone();
            let repaint = repaint.clone();
            let cb = move |_ts: u64, bytes: &[u8], _: &mut ()| {
                let Some(event) = MidiEvent::parse(bytes) else {
                    return;
                };
                activity.store(epoch.elapsed().as_millis().max(1) as u64, Relaxed);
                let t = Instant::now();
                if let Ok(mut c) = ctrl.lock() {
                    lock_wait.fetch_max(t.elapsed().as_micros() as u64, Relaxed);
                    c.handle_input(&InputEvent::Midi {
                        port: port_name.clone(),
                        event,
                    });
                }
                repaint();
            };
            match input.connect(&port, "bubble-in", cb, ()) {
                Ok(conn) => me.connections.push((name, conn)),
                Err(e) => me.errors.push(format!("{name}: {e}")),
            }
        }
        me
    }

    pub fn open_ports(&self) -> Vec<&str> {
        self.connections.iter().map(|(n, _)| n.as_str()).collect()
    }

    pub fn active_recently(&self, ms: u64) -> bool {
        let last = self.last_activity_ms.load(Relaxed);
        last != 0 && (self.epoch.elapsed().as_millis() as u64).saturating_sub(last) < ms
    }
}
