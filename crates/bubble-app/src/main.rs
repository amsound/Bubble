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
    // The main window opens where it was last closed.
    let place = prefs::Prefs::load().window.filter(|w| w.usable());
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Bubble")
        .with_inner_size(place.map_or([1200.0, 760.0], |w| [w.w, w.h]));
    if let Some(w) = place {
        viewport = viewport
            .with_position([w.x, w.y])
            .with_maximized(w.maximized);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
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
