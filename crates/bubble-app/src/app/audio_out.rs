//! Output streams: opening them, surviving a device that disappears, and
//! reconnecting when it returns.
//!
//! When a device fails (a Dante Virtual Soundcard switched off, a USB interface
//! unplugged), the streams on the other devices keep running — carts on them carry
//! on — and the lost device's outputs move to the system default output. The device
//! list is checked every couple of seconds; once the device is back and nothing is
//! playing, its outputs return to it.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use bubble_control::Route;
use bubble_engine::device::{AudioOutput, OutputRequest, StreamProblem, list_output_devices};

use super::BubbleApp;

/// How often the device list is checked while a device is missing.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Devices lost or never opened, and the background check for their return.
#[derive(Default)]
pub(super) struct DeviceWatch {
    /// Configured devices that are not open (by name as listed).
    pub missing: Vec<String>,
    /// Where their outputs play meanwhile ("MacBook Pro Speakers").
    pub fallback: Option<String>,
    /// A missing device has reappeared; reconnect when nothing is playing.
    pub back: bool,
    /// Outputs (0 = A) playing on the fallback instead of their own device.
    pub rerouted: Vec<usize>,
    pending: Option<mpsc::Receiver<Vec<String>>>,
    last: Option<Instant>,
}

impl BubbleApp {
    /// Opens every output afresh (settings changed, or "Reopen audio output").
    pub(super) fn open_audio(&mut self) {
        self.rebuild_audio(&[], false);
    }

    /// Opens the streams the enabled outputs need, all at one sample rate, and routes
    /// outputs A, B, … to them. With `keep`, streams still running (other than
    /// those listed in `dead`, indices into `self.audio`) are kept, so carts playing
    /// on them are not interrupted. Outputs whose device cannot be opened play on
    /// the system default output.
    pub(super) fn rebuild_audio(&mut self, dead: &[usize], keep: bool) {
        let outputs = self.prefs.effective_outputs();
        let rate = self.prefs.rate();
        // Devices in order of first use, with the channels they must provide.
        let mut devices: Vec<(String, u16)> = Vec::new();
        for o in &outputs {
            let need = o.first_channel + o.channels;
            match devices.iter_mut().find(|(d, _)| *d == o.device) {
                Some(d) => d.1 = d.1.max(need),
                None => devices.push((o.device.clone(), need)),
            }
        }

        // Current streams with their engines; the dead (and, without `keep`, all)
        // are dropped before anything is opened so their devices are free.
        let engines = self.lock().detach_engines();
        let keys = std::mem::take(&mut self.audio_keys);
        let streams = std::mem::take(&mut self.audio);
        let mut old: Vec<_> = keys
            .into_iter()
            .zip(streams)
            .zip(engines)
            .enumerate()
            .filter(|(i, ((key, _), _))| {
                keep && !dead.contains(i)
                    // The system default may be needed as the fallback.
                    && (key.is_empty() || devices.iter().any(|(d, _)| d == key))
            })
            .map(|(_, ((key, out), engine))| (key, out, engine))
            .collect();

        self.output_errors.clear();
        self.audio_error = None;
        let mut engines = Vec::new();
        let mut opened: Vec<Option<usize>> = Vec::new(); // device index → engine index
        let mut failed: Vec<String> = Vec::new();
        let req = |name: &str, need: u16| OutputRequest {
            device_name: (!name.is_empty()).then(|| name.to_string()),
            sample_rate: Some(rate),
            min_channels: Some(need),
            buffer_frames: self.prefs.buffer_frames,
            voices: None,
        };
        for (name, need) in &devices {
            if let Some(i) = old
                .iter()
                .position(|(k, out, _)| k == name && out.channels >= *need)
            {
                let (k, out, engine) = old.remove(i);
                opened.push(Some(engines.len()));
                engines.push(engine);
                self.audio.push(out);
                self.audio_keys.push(k);
                continue;
            }
            match AudioOutput::open(&req(name, *need)) {
                Ok((out, engine)) => {
                    opened.push(Some(engines.len()));
                    engines.push(engine);
                    self.audio.push(out);
                    self.audio_keys.push(name.clone());
                }
                Err(e) => {
                    opened.push(None);
                    failed.push(name.clone());
                    self.output_errors
                        .push(format!("{}: {e}", shown_device(name)));
                }
            }
        }

        // The system default output stands in for devices that failed.
        let mut fallback = None;
        if !failed.is_empty() {
            fallback = self.audio_keys.iter().position(|k| k.is_empty());
            if fallback.is_none() {
                let reused = old.iter().position(|(k, _, _)| k.is_empty());
                let got = match reused {
                    Some(i) => {
                        let (_, out, engine) = old.remove(i);
                        Some((out, engine))
                    }
                    None => AudioOutput::open(&req("", 2)).ok(),
                };
                if let Some((out, engine)) = got {
                    fallback = Some(engines.len());
                    engines.push(engine);
                    self.audio.push(out);
                    self.audio_keys.push(String::new());
                }
            }
        }
        drop(old);

        self.watch.missing = failed.iter().filter(|n| !n.is_empty()).cloned().collect();
        self.watch.back = false;
        self.watch.rerouted.clear();
        self.watch.fallback = fallback.map(|i| self.audio[i].device_name.clone());
        if engines.is_empty() {
            self.audio_error = Some(
                self.output_errors
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "no output device".into()),
            );
            return;
        }
        let routes = outputs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let d = devices
                    .iter()
                    .position(|(n, _)| *n == o.device)
                    .unwrap_or(0);
                match opened[d] {
                    Some(engine) => Route {
                        engine,
                        offset: o.first_channel,
                        channels: o.channels,
                    },
                    None => {
                        self.watch.rerouted.push(i);
                        let letter = bubble_project::output_letter(i);
                        let to = match &self.watch.fallback {
                            Some(f) => format!("plays on {f} until it returns"),
                            None => "plays on Output A".into(),
                        };
                        self.output_errors.push(format!(
                            "Output {letter} ({}) {to}",
                            shown_device(&o.device)
                        ));
                        Route {
                            engine: fallback.unwrap_or(0),
                            offset: 0,
                            channels: 2,
                        }
                    }
                }
            })
            .collect();
        self.lock().attach_outputs(engines, routes);
    }

    /// Once per frame: reacts to failed streams and reconnects returning devices.
    pub(super) fn watch_audio(&mut self, ctx: &eframe::egui::Context) {
        let mut dead = Vec::new();
        let mut lost = Vec::new();
        for (i, a) in self.audio.iter().enumerate() {
            for p in a.take_problems() {
                if let StreamProblem::Fatal(msg) = p
                    && !dead.contains(&i)
                {
                    dead.push(i);
                    lost.push(format!("Audio stopped on {}: {msg}", a.device_name));
                }
            }
        }
        if !dead.is_empty() {
            self.rebuild_audio(&dead, true);
            self.output_errors.splice(0..0, lost);
        }

        if self.watch.missing.is_empty() {
            self.watch.pending = None;
            return;
        }
        // Ask for the device list off the UI thread; enumeration can be slow.
        if let Some(rx) = &self.watch.pending {
            match rx.try_recv() {
                Ok(names) => {
                    self.watch.pending = None;
                    if self.watch.missing.iter().any(|m| names.contains(m)) {
                        self.watch.back = true;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.watch.pending = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        } else if !self.watch.back && self.watch.last.is_none_or(|t| t.elapsed() >= WATCH_EVERY) {
            self.watch.last = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            let repaint = self.repaint.clone();
            std::thread::spawn(move || {
                let names = list_output_devices()
                    .map(|l| l.into_iter().map(|d| d.name).collect())
                    .unwrap_or_default();
                let _ = tx.send(names);
                repaint();
            });
            self.watch.pending = Some(rx);
        }
        // Switch back only between cues, so nothing playing is cut off.
        if self.watch.back && !self.lock().any_voice() {
            self.rebuild_audio(&[], true);
        }
        ctx.request_repaint_after(WATCH_EVERY);
    }

    /// Whether carts on `output` ("A", "B", …) are playing on the fallback.
    /// Also true for a letter not set up on this machine (it plays on Output A).
    pub(super) fn is_rerouted(&self, output: &str) -> bool {
        bubble_project::output_index(output)
            .is_some_and(|i| !self.output_set_up(i) || self.watch.rerouted.contains(&i))
    }

    /// Whether output `index` (0 = A) is set up on this machine.
    fn output_set_up(&self, index: usize) -> bool {
        index < self.prefs.outputs.len().max(1)
    }

    /// Where a rerouted output is playing, for the tile's tooltip.
    pub(super) fn reroute_text(&self, output: &str) -> Option<String> {
        if !self.is_rerouted(output) {
            return None;
        }
        if !self.output_set_up(bubble_project::output_index(output)?) {
            return Some(format!(
                "Output {output} is not set up on this computer, so this cart plays on \
                 Output A. Add it in Audio → Outputs, or move carts with Audio → Reassign \
                 Cart Outputs."
            ));
        }
        let device = self
            .prefs
            .effective_outputs()
            .get(bubble_project::output_index(output)?)
            .map(|o| o.device.clone())
            .unwrap_or_default();
        Some(format!(
            "{} is unavailable: Output {output} is playing on {}",
            if device.is_empty() {
                "Its device"
            } else {
                &device
            },
            self.watch.fallback.as_deref().unwrap_or("Output A")
        ))
    }

    /// Status-bar text while a device is missing.
    pub(super) fn audio_notice(&self) -> Option<String> {
        let lost = self.watch.missing.join(", ");
        if lost.is_empty() {
            return None;
        }
        Some(match (&self.watch.fallback, self.watch.back) {
            (_, true) => format!("{lost} is back; reconnecting when nothing is playing"),
            (Some(f), false) => format!("{lost} unavailable; playing on {f}"),
            (None, false) => format!("{lost} unavailable"),
        })
    }
}

fn shown_device(name: &str) -> &str {
    if name.is_empty() {
        "System default"
    } else {
        name
    }
}
