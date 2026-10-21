//! The main window and the operating system, through Slint's winit accessor
//! (`unstable-winit-030`): its place on screen, the title bar's light/dark theme,
//! interface size, bringing it to the front, and where the pointer is during an OS
//! file drag.

use bubble_core::prefs::{Appearance, WindowPlace};
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit;

use crate::MainWindow;

/// The window's current place in logical points (not counting the interface size),
/// or `None` while it is minimised or full screen.
pub fn place(ui: &MainWindow) -> Option<WindowPlace> {
    ui.window()
        .with_winit_window(|w| {
            if w.is_minimized() == Some(true) || w.fullscreen().is_some() {
                return None;
            }
            let s = w.scale_factor();
            let pos = w.outer_position().ok()?.to_logical::<f32>(s);
            let size = w.inner_size().to_logical::<f32>(s);
            Some(WindowPlace {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
                maximized: w.is_maximized(),
            })
        })
        .flatten()
}

/// Opens the window where it was last closed (call before showing it).
pub fn restore_place(ui: &MainWindow, place: Option<WindowPlace>) {
    let Some(p) = place.filter(WindowPlace::usable) else {
        return;
    };
    let w = ui.window();
    w.set_size(slint::LogicalSize::new(p.w, p.h));
    w.set_position(slint::LogicalPosition::new(p.x, p.y));
    if p.maximized {
        w.set_maximized(true);
    }
}

/// After a second monitor is unplugged, a restored place can be off every screen:
/// then the window is centred on the primary screen.
pub fn keep_on_screen(ui: &MainWindow) {
    ui.window().with_winit_window(|w| {
        let Ok(pos) = w.outer_position() else { return };
        // A point on the title bar must be on some monitor.
        let s = w.scale_factor();
        let probe = (pos.x + (60.0 * s) as i32, pos.y + (12.0 * s) as i32);
        let on_screen = w.available_monitors().any(|m| {
            let (p, z) = (m.position(), m.size());
            probe.0 >= p.x
                && probe.1 >= p.y
                && probe.0 < p.x + z.width as i32
                && probe.1 < p.y + z.height as i32
        });
        if on_screen {
            return;
        }
        let Some(m) = w
            .primary_monitor()
            .or_else(|| w.available_monitors().next())
        else {
            return;
        };
        let (mp, ms, ws) = (m.position(), m.size(), w.outer_size());
        let x = mp.x + (ms.width as i32 - ws.width as i32).max(0) / 2;
        let y = mp.y + (ms.height as i32 - ws.height as i32).max(0) / 2;
        bubble_core::log::info("saved window place is off every screen; centring it");
        w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    });
}

/// Whether the system uses a dark appearance (`None` before the window exists).
pub fn system_dark(ui: &MainWindow) -> Option<bool> {
    ui.window()
        .with_winit_window(|w| w.theme())
        .flatten()
        .map(|t| t == winit::window::Theme::Dark)
}

/// The title bar follows Bubble's appearance, not only the system's (W4).
pub fn set_title_bar_theme(ui: &MainWindow, a: Appearance) {
    ui.window().with_winit_window(|w| {
        w.set_theme(match a {
            Appearance::System => None,
            Appearance::Light => Some(winit::window::Theme::Light),
            Appearance::Dark => Some(winit::window::Theme::Dark),
        })
    });
}

/// View ▸ Interface Size: Slint's scale factor is the system's times `zoom`.
pub fn apply_zoom(ui: &MainWindow, zoom: f32) {
    let Some(os) = ui.window().with_winit_window(|w| w.scale_factor()) else {
        return;
    };
    let scale_factor = (os as f32 * zoom).clamp(0.5, 6.0);
    if (ui.window().scale_factor() - scale_factor).abs() > 1e-3 {
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor });
    }
}

/// Another launch asked Bubble to show itself.
pub fn bring_to_front(ui: &MainWindow) {
    ui.window().with_winit_window(|w| {
        w.set_minimized(false);
        w.set_visible(true);
        w.focus_window();
    });
}

/// Where the pointer is in the window (Slint's logical coordinates), from the
/// system: winit reports dropped files without a position, and macOS sends no mouse
/// moves during a drag.
pub fn pointer_in_window(ui: &MainWindow) -> Option<(f32, f32)> {
    use mouse_position::mouse_position::Mouse;
    let Mouse::Position { x, y } = Mouse::get_mouse_position() else {
        return None;
    };
    let (inner, os) = ui
        .window()
        .with_winit_window(|w| w.inner_position().ok().map(|p| (p, w.scale_factor())))
        .flatten()?;
    // macOS reports points; Windows and X11 report pixels.
    let (px, py) = if cfg!(target_os = "macos") {
        (x as f64 * os, y as f64 * os)
    } else {
        (x as f64, y as f64)
    };
    let s = ui.window().scale_factor() as f64;
    Some((
        ((px - inner.x as f64) / s) as f32,
        ((py - inner.y as f64) / s) as f32,
    ))
}

/// The window icon, rendered from `assets/logo.svg` by the build script.
pub fn icon() -> slint::Image {
    let rgba: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba"));
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba, 256, 256);
    slint::Image::from_rgba8(buf)
}
