//! The "Convert to WAV?" offer (the conversion itself is in `bubble_core::app::convert`).
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

use eframe::egui;

use super::BubbleApp;

impl BubbleApp {
    /// "Convert N files to WAV?" after turning the setting on or opening a session.
    pub(super) fn convert_offer_modal(&mut self, ctx: &egui::Context) {
        let Some(files) = &self.prompts.convert_offer else {
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
            let files = self.prompts.convert_offer.take().unwrap_or_default();
            self.start_conversion(files);
        } else if later || resp.should_close() {
            self.prompts.convert_offer = None;
        }
    }
}
