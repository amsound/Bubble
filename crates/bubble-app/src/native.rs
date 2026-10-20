//! Secondary windows as real OS windows (title bar, close button and keyboard
//! focus from the platform: on macOS the close button is on the left, as users
//! expect), instead of panels drawn inside the main window.

use eframe::egui::{self, ViewportBuilder, ViewportId};

pub struct WindowSpec<'a> {
    pub key: &'a str,
    pub title: &'a str,
    pub size: [f32; 2],
    pub min_size: [f32; 2],
    pub on_top: bool,
    /// Size the window's height to its content when it opens (small dialogs).
    pub fit: bool,
}

/// The standard bottom row of a dialog: a separator line, then buttons on the
/// right. Add them primary first (it ends up rightmost).
pub fn dialog_bar<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(2.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add)
        .inner
}

/// The blue default button of a dialog.
pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_string())
            .strong()
            .color(egui::Color32::WHITE),
    )
    .fill(egui::Color32::from_rgb(0x2F, 0x6B, 0xFF))
    .min_size(egui::Vec2::new(96.0, 30.0))
}

/// A secondary dialog button, the same size as [`primary`].
pub fn secondary(text: &str) -> egui::Button<'static> {
    egui::Button::new(text.to_string()).min_size(egui::Vec2::new(90.0, 30.0))
}

/// Shows a native window for this frame. Returns `(still_open, focused)`.
/// The window stays open as long as the caller keeps calling this each frame.
pub fn native_window(
    ctx: &egui::Context,
    spec: WindowSpec<'_>,
    add: impl FnOnce(&mut egui::Ui),
) -> (bool, bool) {
    let mut builder = ViewportBuilder::default()
        .with_title(spec.title)
        .with_inner_size(spec.size)
        .with_min_inner_size(spec.min_size)
        .with_resizable(true);
    if spec.on_top {
        builder = builder.with_always_on_top();
    }
    let mut add = Some(add);
    let fit = spec.fit;
    let key = spec.key.to_string();
    ctx.show_viewport_immediate(ViewportId::from_hash_of(spec.key), builder, |ui, _class| {
        let mut used = 0.0;
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(f) = add.take() {
                f(ui);
            }
            used = ui.min_rect().height();
        });
        if fit {
            // Fit once per opening: remember the pass we last drew on; a gap means
            // the window was closed in between.
            let id = egui::Id::unique(("fit", &key));
            let pass = ui.ctx().cumulative_pass_nr();
            let last: Option<(u64, bool)> = ui.ctx().data(|d| d.get_temp(id));
            let fitted = matches!(last, Some((p, true)) if p + 2 >= pass);
            if !fitted && used > 0.0 {
                let w = ui
                    .ctx()
                    .input(|i| i.viewport().inner_rect.map_or(400.0, |r| r.width()));
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::new(
                        w,
                        used + 18.0,
                    )));
            }
            ui.ctx().data_mut(|d| d.insert_temp(id, (pass, used > 0.0)));
        }
        let (close, focused) = ui.ctx().input(|i| {
            (
                i.viewport().close_requested()
                    || (i.modifiers.command && i.key_pressed(egui::Key::W)),
                i.focused,
            )
        });
        (!close, focused)
    })
}
