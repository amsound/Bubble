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
use bubble_control::assets::{Resolved, resolve_all};
use bubble_engine::EngineHandle;
use bubble_engine::device::{AudioOutput, OutputRequest, StreamProblem, list_output_devices};

use super::Core;

/// How often the device list is checked while a device is missing.
pub const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Devices lost or never opened, and the background check for their return.
#[derive(Default)]
pub struct DeviceWatch {
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
    /// Streams being opened in the background.
    job: Option<Rebuild>,
    /// Another rebuild was asked for meanwhile (with `keep` or not).
    again: Option<bool>,
    /// Why streams stopped, shown once the rebuild is done.
    lost: Vec<String>,
    tasks: Option<mpsc::Sender<AudioTask>>,
}

/// A stream and its engine on their way out.
type Retired = Vec<(AudioOutput, EngineHandle)>;
/// What the helper thread opened, by device.
type Opened = Vec<(String, Result<(AudioOutput, EngineHandle), String>)>;
type AudioTask = Box<dyn FnOnce() + Send>;

struct Rebuild {
    rx: mpsc::Receiver<(Opened, Option<Resolved>)>,
    outputs: Vec<crate::prefs::OutputPref>,
    devices: Vec<(String, u16)>,
    dead: Vec<usize>,
    keep: bool,
    since: Instant,
}

fn log_opened(a: &AudioOutput) {
    crate::log::info(&format!(
        "audio output: {} at {} Hz, {} ch, {}",
        a.device_name, a.sample_rate, a.channels, a.sample_format
    ));
    if let Some(n) = &a.rate_note {
        crate::log::info(&format!("audio device {}: {n}", a.device_name));
    }
    if let Some(d) = a.device_rate.filter(|d| *d != a.sample_rate) {
        crate::log::info(&format!(
            "audio device {} runs at {d} Hz, Bubble at {} Hz: Windows converts",
            a.device_name, a.sample_rate
        ));
    }
}

impl Core {
    /// Opens every output afresh (settings changed, or "Reopen audio output").
    pub fn open_audio(&mut self) {
        self.rebuild_audio(&[], false);
    }

    /// Whether output streams are being opened in the background.
    pub fn audio_opening(&self) -> bool {
        self.watch.job.is_some()
    }

    /// Opens the streams the enabled outputs need, all at one sample rate, and routes
    /// outputs A, B, … to them. With `keep`, streams still running (other than
    /// those listed in `dead`, indices into `self.audio`) are kept, so carts playing
    /// on them are not interrupted. Outputs whose device cannot be opened play on
    /// the system default output.
    ///
    /// Opening a device can take seconds and changing its rate longer, so it happens on
    /// a helper thread and `finish_rebuild` takes the result. With `keep`, the current
    /// engines stay attached meanwhile, so triggers and stops still reach them.
    pub fn rebuild_audio(&mut self, dead: &[usize], keep: bool) {
        if let Some(job) = &mut self.watch.job {
            job.dead.extend_from_slice(dead);
            self.watch.again = Some(self.watch.again.unwrap_or(true) && keep);
            return;
        }
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

        // Without `keep`, everything goes first, so the devices are free.
        let mut retire: Retired = Vec::new();
        if !keep {
            let engines = self.lock().detach_engines();
            self.audio_keys.clear();
            retire.extend(std::mem::take(&mut self.audio).into_iter().zip(engines));
        }
        let alive = |i: &usize| !dead.contains(i);
        let usable = |name: &str, need: u16| {
            self.audio_keys
                .iter()
                .zip(&self.audio)
                .enumerate()
                .any(|(i, (k, out))| alive(&i) && k == name && out.channels >= need)
        };
        let to_open: Vec<(String, u16)> = devices
            .iter()
            .filter(|(name, need)| !usable(name, *need))
            .cloned()
            .collect();
        let has_default = self
            .audio_keys
            .iter()
            .enumerate()
            .any(|(i, k)| alive(&i) && k.is_empty());
        let buffer_frames = self.prefs.buffer_frames;
        // A new rate re-arms all the audio: its files are looked up on the helper thread.
        let to_resolve = {
            let c = self.lock();
            (c.prepared_rate() != rate).then(|| (c.project().clone(), c.project_dir()))
        };
        let repaint = self.repaint.clone();
        let (tx, rx) = mpsc::channel();
        self.watch.job = Some(Rebuild {
            rx,
            outputs,
            devices,
            dead: dead.to_vec(),
            keep,
            since: Instant::now(),
        });
        self.audio_task(Box::new(move || {
            drop(retire);
            let req = |name: &str, need: u16| OutputRequest {
                device_name: (!name.is_empty()).then(|| name.to_string()),
                sample_rate: Some(rate),
                min_channels: Some(need),
                buffer_frames,
                voices: None,
            };
            let open = |name: &str, need: u16| {
                AudioOutput::open(&req(name, need)).map_err(|e| e.to_string())
            };
            let mut opened: Opened = Vec::new();
            for (name, need) in &to_open {
                opened.push((name.clone(), open(name, *need)));
            }
            // The system default output stands in for devices that failed.
            let failed = opened.iter().any(|(_, r)| r.is_err());
            let default_open = has_default || opened.iter().any(|(n, r)| n.is_empty() && r.is_ok());
            if failed && !default_open {
                opened.push((String::new(), open("", 2)));
            }
            let found = to_resolve.map(|(project, dir)| resolve_all(&project, dir.as_deref()));
            let _ = tx.send((opened, found));
            repaint();
        }));
    }

    /// Runs `task` on the audio helper thread, which stays alive: streams are made
    /// there, and some audio objects belong to the thread that made them.
    fn audio_task(&mut self, task: AudioTask) {
        let task = match &self.watch.tasks {
            Some(tx) => match tx.send(task) {
                Ok(()) => return,
                Err(e) => e.0,
            },
            None => task,
        };
        let (tx, rx) = mpsc::channel::<AudioTask>();
        let spawned = std::thread::Builder::new()
            .name("bubble-audio-open".into())
            .spawn(move || {
                for task in rx {
                    task();
                }
            });
        match spawned {
            Ok(_) => {
                let _ = tx.send(task);
                self.watch.tasks = Some(tx);
            }
            Err(e) => {
                crate::log::info(&format!("no audio helper thread: {e}"));
                task();
            }
        }
    }

    /// Takes the streams opened in the background and routes the outputs to them, all
    /// under one hold of the session's lock.
    fn finish_rebuild(&mut self, job: Rebuild, mut results: Opened) {
        crate::log::info(&format!(
            "audio outputs opened in {:.2} s",
            job.since.elapsed().as_secs_f32()
        ));
        let Rebuild {
            outputs,
            devices,
            dead,
            keep,
            ..
        } = job;
        let ctrl = self.ctrl.clone();
        let mut c = ctrl.lock().unwrap_or_else(|p| p.into_inner());
        let engines = c.detach_engines();
        let keys = std::mem::take(&mut self.audio_keys);
        let streams = std::mem::take(&mut self.audio);
        let mut retire: Retired = Vec::new();
        let mut old = Vec::new();
        for (i, ((key, out), engine)) in keys.into_iter().zip(streams).zip(engines).enumerate() {
            // The system default may be needed as the fallback.
            if keep
                && !dead.contains(&i)
                && (key.is_empty() || devices.iter().any(|(d, _)| *d == key))
            {
                old.push((key, out, engine));
            } else {
                retire.push((out, engine));
            }
        }
        let mut take = |name: &str| {
            results
                .iter()
                .position(|(n, _)| n == name)
                .map(|i| results.remove(i).1)
        };

        self.output_errors.clear();
        self.audio_error = None;
        let mut engines = Vec::new();
        let mut opened: Vec<Option<usize>> = Vec::new(); // device index → engine index
        let mut failed: Vec<String> = Vec::new();
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
            match take(name) {
                Some(Ok((out, engine))) => {
                    log_opened(&out);
                    opened.push(Some(engines.len()));
                    engines.push(engine);
                    self.audio.push(out);
                    self.audio_keys.push(name.clone());
                }
                Some(Err(e)) => {
                    opened.push(None);
                    failed.push(name.clone());
                    self.output_errors
                        .push(format!("{}: {e}", shown_device(name)));
                }
                // It died after the job began; the job that follows opens it.
                None => {
                    opened.push(None);
                    failed.push(name.clone());
                    self.output_errors
                        .push(format!("{}: reopening", shown_device(name)));
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
                    None => take("").and_then(Result::ok),
                };
                if let Some((out, engine)) = got {
                    log_opened(&out);
                    fallback = Some(engines.len());
                    engines.push(engine);
                    self.audio.push(out);
                    self.audio_keys.push(String::new());
                }
            }
        }
        retire.extend(old.into_iter().map(|(_, out, engine)| (out, engine)));

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
        } else {
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
            c.attach_outputs(engines, routes);
        }
        drop(c);
        let lost = std::mem::take(&mut self.watch.lost);
        self.output_errors.splice(0..0, lost);
        for e in self.output_errors.iter().chain(self.audio_error.iter()) {
            crate::log::info(&format!("audio problem: {e}"));
        }
        // A stream that is gone may wait on its device while closing: not on this thread.
        if !retire.is_empty() {
            self.audio_task(Box::new(move || drop(retire)));
        }
        if let Some(keep) = self.watch.again.take() {
            self.rebuild_audio(&[], keep);
        }
    }

    /// Open devices the system runs at another rate than Bubble's mixer (Windows,
    /// where setting it failed): (device key as in the outputs, its name, its rate).
    /// Their audio is converted twice, once by Bubble and again by Windows.
    pub fn rate_mismatches(&self) -> Vec<(String, String, u32)> {
        self.audio_keys
            .iter()
            .zip(&self.audio)
            .filter_map(|(k, a)| {
                a.device_rate
                    .filter(|d| *d != a.sample_rate)
                    .map(|d| (k.clone(), a.device_name.clone(), d))
            })
            .collect()
    }

    /// Once per frame: reacts to failed streams and reconnects returning devices.
    /// Returns when it next needs to run while a device is missing (the interface
    /// should wake by then).
    pub fn watch_audio(&mut self) -> Option<Duration> {
        if let Some(result) = self.watch.job.as_ref().map(|j| j.rx.try_recv()) {
            match result {
                Ok((opened, found)) => {
                    if let Some(job) = self.watch.job.take() {
                        if let Some(found) = found {
                            self.lock().use_resolved(found);
                        }
                        self.finish_rebuild(job, opened);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The helper thread died opening them: nothing arrived.
                    if let Some(job) = self.watch.job.take() {
                        crate::log::info("the audio helper thread stopped");
                        self.watch.tasks = None;
                        self.finish_rebuild(job, Vec::new());
                    }
                }
            }
        }
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
            self.watch.lost.extend(lost);
            self.rebuild_audio(&dead, true);
        }

        if self.watch.missing.is_empty() {
            self.watch.pending = None;
            return None;
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
        if self.watch.back && self.watch.job.is_none() && !self.lock().any_voice() {
            self.rebuild_audio(&[], true);
        }
        Some(WATCH_EVERY)
    }

    /// Whether carts on `output` ("A", "B", …) are playing on the fallback.
    /// Also true for a letter not set up on this machine (it plays on Output A).
    pub fn is_rerouted(&self, output: &str) -> bool {
        bubble_project::output_index(output)
            .is_some_and(|i| !self.output_set_up(i) || self.watch.rerouted.contains(&i))
    }

    /// Whether output `index` (0 = A) is set up on this machine.
    pub fn output_set_up(&self, index: usize) -> bool {
        index < self.prefs.outputs.len().max(1)
    }

    /// Where a rerouted output is playing, for the tile's tooltip.
    pub fn reroute_text(&self, output: &str) -> Option<String> {
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
    pub fn audio_notice(&self) -> Option<String> {
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
