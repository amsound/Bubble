//! The session setting "Audio files: Convert to WAV".
//!
//! Bubble always plays from memory, decoded once when a session opens. With this
//! setting, files are converted once instead — to 32-bit float WAV at the engine's
//! sample rate, next to the originals in the session's Audio folder — so opening the
//! session later needs no decoding or resampling. That is what makes the difference
//! on slower computers; playback itself costs the same either way.
//!
//! Audio added while the setting is on is converted straight away. Turning the
//! setting on, or opening a session that has it on, offers to convert the files
//! that are not converted yet.

use std::path::{Path, PathBuf};

use bubble_control::Conversion;
use bubble_engine::loader::{convert_to_wav, needs_wav_conversion};
use bubble_project::session::AUDIO_DIR;
use eframe::egui;
use uuid::Uuid;

use super::BubbleApp;
use super::session_ui::JobResult;

impl BubbleApp {
    /// Asks for a conversion check once nothing else is busy. `ask` offers the
    /// conversion first; otherwise it simply runs (audio just added).
    pub(super) fn want_wav_check(&mut self, ask: bool) {
        self.wav_check = Some(self.wav_check.unwrap_or(false) || ask);
    }

    /// Runs a pending check when the session is idle (no job, open or collection).
    pub(super) fn wav_check_when_idle(&mut self) {
        let Some(ask) = self.wav_check else { return };
        if self.job.is_some() || self.opening.is_some() || self.want_collect {
            return;
        }
        self.wav_check = None;
        let (on, dir) = {
            let c = self.lock();
            (c.project().settings.convert_to_wav, c.project_dir())
        };
        if !on || dir.is_none() {
            return;
        }
        let files = self.wav_candidates();
        if files.is_empty() {
            return;
        }
        if ask {
            self.dialogs.convert_offer = Some(files);
        } else {
            self.start_conversion(files);
        }
    }

    /// Assets whose files are not yet WAV at the engine rate.
    fn wav_candidates(&self) -> Vec<(Uuid, PathBuf)> {
        let rate = self.prefs.rate();
        self.lock()
            .asset_files()
            .into_iter()
            .filter(|(_, p)| needs_wav_conversion(p, rate))
            .collect()
    }

    fn start_conversion(&mut self, files: Vec<(Uuid, PathBuf)>) {
        let Some(dir) = self.lock().project_dir() else {
            return;
        };
        let rate = self.prefs.rate();
        let n = files.len();
        self.start_job(
            format!(
                "Converting {n} file{} to WAV…",
                if n == 1 { "" } else { "s" }
            ),
            move || {
                let audio = dir.join(AUDIO_DIR);
                let mut done = Vec::new();
                let mut errors = Vec::new();
                if let Err(e) = std::fs::create_dir_all(&audio) {
                    return JobResult::Failed(format!(
                        "Could not create {}:\n{e}",
                        audio.display()
                    ));
                }
                for (asset, src) in files {
                    let dst = wav_name(&audio, &src, rate);
                    match convert_to_wav(&src, &dst, rate) {
                        Ok(info) => done.push(Conversion {
                            asset,
                            file: dst,
                            from_rate: info.sample_rate,
                            to_rate: rate,
                        }),
                        Err(e) => errors.push(format!("{}: {e}", src.display())),
                    }
                }
                JobResult::Converted(done, errors)
            },
        );
    }

    /// "Convert N files to WAV?" after turning the setting on or opening a session.
    pub(super) fn convert_offer_modal(&mut self, ctx: &egui::Context) {
        let Some(files) = &self.dialogs.convert_offer else {
            return;
        };
        let n = files.len();
        let rate = self.prefs.rate() as f32 / 1000.0;
        let mut convert = false;
        let mut later = false;
        let resp = egui::Modal::new(egui::Id::unique("convert-offer")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Convert to WAV");
            ui.add_space(6.0);
            ui.label(format!(
                "{n} audio file{} in this session {} not WAV at {rate} kHz yet. Convert \
                 {} now? The originals stay in the Audio folder.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "is" } else { "are" },
                if n == 1 { "it" } else { "them" },
            ));
            crate::native::dialog_bar(ui, |ui| {
                convert = ui.add(crate::native::primary("Convert")).clicked();
                later = ui.add(crate::native::secondary("Not Now")).clicked();
            });
        });
        if convert {
            let files = self.dialogs.convert_offer.take().unwrap_or_default();
            self.start_conversion(files);
        } else if later || resp.should_close() {
            self.dialogs.convert_offer = None;
        }
    }
}

/// A free name in `audio` for the WAV made from `src`: "Sting.wav" for
/// "Sting.mp3", "Sting 48k.wav" for a WAV at another rate, numbered if taken.
fn wav_name(audio: &Path, src: &Path, rate: u32) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio".into());
    let was_wav = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("wave"));
    let base = if was_wav {
        format!("{stem} {}k", rate / 1000)
    } else {
        stem
    };
    (1u32..)
        .map(|n| {
            if n == 1 {
                audio.join(format!("{base}.wav"))
            } else {
                audio.join(format!("{base} ({n}).wav"))
            }
        })
        .find(|p| !p.exists())
        .expect("a free name")
}

#[cfg(test)]
mod tests {
    use super::wav_name;

    #[test]
    fn wav_names_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("bubble-wavname-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mp3 = dir.join("Sting.mp3");
        assert_eq!(wav_name(&dir, &mp3, 48_000), dir.join("Sting.wav"));
        std::fs::write(dir.join("Sting.wav"), b"x").unwrap();
        assert_eq!(wav_name(&dir, &mp3, 48_000), dir.join("Sting (2).wav"));
        let wav = dir.join("Bed.wav");
        assert_eq!(wav_name(&dir, &wav, 48_000), dir.join("Bed 48k.wav"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
