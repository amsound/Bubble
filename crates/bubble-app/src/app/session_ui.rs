//! Sessions: creating them, opening anything (Bubble sessions, SpotOn sessions and
//! packages), and keeping every audio file inside the session's `Audio` folder.
//!
//! File copies and imports run on a helper thread (`Job`) so large files never stall
//! the interface; the result is applied on a later frame.

use eframe::egui;

use bubble_core::app::AfterCreate;
use bubble_core::app::session::file_name;
use bubble_core::dialogs::Purpose;

use super::{BubbleApp, Intent, PendingAction, Snapshot};

impl BubbleApp {
    /// "Add to current session or new session?" for a SpotOn session or package.
    pub(super) fn import_choice_modal(
        &mut self,
        ctx: &egui::Context,
        snap: &Snapshot,
        intents: &mut Vec<Intent>,
    ) {
        let Some(path) = self.prompts.import_choice.clone() else {
            return;
        };
        let mut add = false;
        let mut new = false;
        let mut cancel = false;
        let resp = egui::Modal::new(egui::Id::unique("import-choice")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading(format!("Import {}", file_name(&path)));
            ui.add_space(8.0);
            crate::native::dialog_bar(ui, |ui| {
                if ui
                    .add(crate::native::primary("Add to Current Session"))
                    .clicked()
                {
                    add = true;
                }
                if ui.add(crate::native::secondary("Cancel")).clicked() {
                    cancel = true;
                }
                if ui.add(crate::native::secondary("New Session…")).clicked() {
                    new = true;
                }
            });
        });
        if add || new || cancel || resp.should_close() {
            self.prompts.import_choice = None;
        }
        if new {
            self.request(PendingAction::Import(path), snap, intents);
        } else if add {
            self.import_into_current(path);
        }
    }

    /// The New Session dialog (modal).
    pub(super) fn new_session_modal(&mut self, ctx: &egui::Context, intents: &mut Vec<Intent>) {
        let Some(ns) = &mut self.prompts.new_session else {
            return;
        };
        let mut create = false;
        let mut cancel = false;
        let mut choose = false;
        let title = match ns.then {
            AfterCreate::Import(_) => "Import into a New Session",
            AfterCreate::SaveAs => "Save Session As",
            _ => "New Session",
        };
        let resp = egui::Modal::new(egui::Id::unique("new-session")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(title);
            ui.add_space(6.0);
            egui::Grid::new("ns-grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    let r = ui.add(egui::TextEdit::singleline(&mut ns.name).desired_width(320.0));
                    if ns.name.is_empty() {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        create = true;
                    }
                    ui.end_row();
                    ui.label("Location");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(ns.location.display().to_string()).weak(),
                            )
                            .truncate(),
                        );
                        if ui.button("Choose…").clicked() {
                            choose = true;
                        }
                    });
                    ui.end_row();
                });
            if let Some(e) = &ns.error {
                ui.colored_label(egui::Color32::from_rgb(0xE5, 0x39, 0x35), e);
            }
            ui.add_space(6.0);
            crate::native::dialog_bar(ui, |ui| {
                let label = match ns.then {
                    AfterCreate::Import(_) => "Import",
                    AfterCreate::SaveAs => "Save",
                    _ => "Create",
                };
                if ui
                    .add_enabled(
                        !bubble_project::session::clean_name(&ns.name).is_empty(),
                        crate::native::primary(label),
                    )
                    .clicked()
                {
                    create = true;
                }
                if ui.add(crate::native::secondary("Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if choose {
            self.files.request(Purpose::ChooseFolder);
        }
        if cancel || resp.should_close() {
            self.prompts.new_session = None;
            return;
        }
        if create {
            self.create_session(intents);
        }
    }
}
