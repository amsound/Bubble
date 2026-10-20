//! Bubble — cart player.

// No console window behind the app on Windows (crash reports go to a file).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod dialogs;
mod fonts;
mod logo;
mod menu;
mod meter;
mod midi;
mod native;
mod prefs;
mod tile;
mod waveform;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Bubble")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true)
            .with_icon(eframe::egui::IconData {
                rgba: include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba")).to_vec(),
                width: 256,
                height: 256,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "Bubble",
        options,
        Box::new(|cc| Ok(Box::new(app::BubbleApp::new(cc)))),
    )
}
