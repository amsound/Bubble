//! MIDI input. Each port's callback runs on midir's thread and goes straight to the
//! controller, then to the engine command ring. The UI is never on this path.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bubble_control::{Controller, InputEvent, MidiEvent};
use midir::{Ignore, MidiInput, MidiInputConnection};

pub struct MidiInputs {
    connections: Vec<(String, MidiInputConnection<()>)>,
    /// Per open port: milliseconds since `epoch` of its last message (0 = never).
    activity: Vec<(String, Arc<AtomicU64>)>,
    /// Longest time a callback waited for the controller lock (µs) — monitors decision D-06.
    pub max_lock_wait_us: Arc<AtomicU64>,
    pub epoch: Instant,
    pub errors: Vec<String>,
}

/// CoreMIDI keeps the process's list of ports up to date on the run loop of the thread
/// that made the first client. Made on a helper thread that then ends, the list stays
/// as it was at start-up, so a device plugged in later never shows. So the first
/// client is made on a thread that lives as long as Bubble and runs its run loop.
#[cfg(target_os = "macos")]
fn keep_port_list_current() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFRunLoopRun();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("bubble-midi-list".into())
            .spawn(move || {
                let client = MidiInput::new("Bubble (ports)");
                let _ = tx.send(());
                if client.is_ok() {
                    // SAFETY: runs this thread's own run loop; it returns only if the
                    // loop has nothing to watch, and then the thread just ends.
                    unsafe { CFRunLoopRun() };
                }
                drop(client);
            });
        if spawned.is_ok() {
            let _ = rx.recv();
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn keep_port_list_current() {}

pub fn available_ports() -> Vec<String> {
    keep_port_list_current();
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
            activity: Vec::new(),
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
            let activity = Arc::new(AtomicU64::new(0));
            me.activity.push((name.clone(), activity.clone()));
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
                // As everywhere: a panic elsewhere must not silence MIDI for good.
                let mut c = ctrl.lock().unwrap_or_else(|p| p.into_inner());
                lock_wait.fetch_max(t.elapsed().as_micros() as u64, Relaxed);
                c.handle_input(&InputEvent::Midi {
                    port: port_name.clone(),
                    event,
                });
                drop(c);
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

    /// Whether any port had a message in the last `ms`.
    pub fn active_recently(&self, ms: u64) -> bool {
        self.activity.iter().any(|(_, a)| self.recent(a, ms))
    }

    /// Whether port `name` had a message in the last `ms`.
    pub fn port_active_recently(&self, name: &str, ms: u64) -> bool {
        self.activity
            .iter()
            .any(|(n, a)| n == name && self.recent(a, ms))
    }

    fn recent(&self, at: &AtomicU64, ms: u64) -> bool {
        let last = at.load(Relaxed);
        last != 0 && (self.epoch.elapsed().as_millis() as u64).saturating_sub(last) < ms
    }
}
