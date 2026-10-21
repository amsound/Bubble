//! Bubble — cart player.

// No console window behind the app on Windows (crash reports go to a file).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod dialogs;
mod fonts;
mod instance;
mod log;
mod logo;
mod menu;
mod meter;
mod midi;
mod native;
mod prefs;
mod tile;
mod waveform;

/// Set on a relaunch with the other renderer, so a second failure is reported
/// instead of relaunching again.
const FALLBACK_ENV: &str = "BUBBLE_RENDERER_FALLBACK";

fn main() -> eframe::Result {
    log::init();
    // Any panic, even before the window exists, is logged (the full crash report
    // hook is installed once the app is up and chains to this one).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::info(&format!("PANIC: {info}"));
        default_hook(info);
    }));

    // One Bubble at a time: a second launch hands its file to the running copy.
    let launch_file = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .find(|p| p.is_file());
    if let instance::Role::Handed = instance::claim(launch_file.as_deref()) {
        log::info(&format!(
            "another copy is running; asked it to show itself{}",
            launch_file.map_or(String::new(), |p| format!(" and open {}", p.display()))
        ));
        return Ok(());
    }

    // The main window opens where it was last closed.
    let place = prefs::Prefs::load()
        .window
        .filter(|w| w.usable() && on_a_screen(w));
    log::info(&format!("preferences loaded; window place {place:?}"));
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Bubble")
        .with_inner_size(place.map_or([1200.0, 760.0], |w| [w.w, w.h]));
    if let Some(w) = place {
        viewport = viewport
            .with_position([w.x, w.y])
            .with_maximized(w.maximized);
    }
    let renderer = renderer();
    log::info(&format!(
        "starting the window with the {renderer:?} renderer"
    ));
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true)
            .with_icon(eframe::egui::IconData {
                rgba: include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba")).to_vec(),
                width: 256,
                height: 256,
            }),
        renderer,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Bubble",
        options,
        Box::new(|cc| {
            log::info("window and renderer ready; building the app");
            Ok(Box::new(app::BubbleApp::new(cc)))
        }),
    );
    match &result {
        Ok(()) => log::info("exited normally"),
        Err(e) => {
            log::info(&format!("could not start with {renderer:?}: {e}"));
            failed_to_start(renderer, e);
        }
    }
    result
}

/// Graphics renderer. macOS: wgpu (Metal). Windows and Linux: OpenGL (glow), which
/// older GPUs (e.g. Intel HD 3000: no DirectX 12 or Vulkan) and virtual machines run in
/// hardware, where wgpu falls back to drawing on the CPU. `BUBBLE_RENDERER=wgpu` or
/// `=glow` overrides it.
fn renderer() -> eframe::Renderer {
    match std::env::var("BUBBLE_RENDERER").as_deref() {
        Ok("wgpu") => eframe::Renderer::Wgpu,
        Ok("glow") => eframe::Renderer::Glow,
        _ if cfg!(target_os = "macos") => eframe::Renderer::Wgpu,
        _ => eframe::Renderer::Glow,
    }
}

/// The window could not be created (e.g. no usable graphics driver). The event loop
/// cannot be started twice in one process, so Bubble relaunches itself once with the
/// other renderer; if that fails too, it says so instead of vanishing.
fn failed_to_start(tried: eframe::Renderer, e: &eframe::Error) {
    if std::env::var_os(FALLBACK_ENV).is_none()
        && let Ok(exe) = std::env::current_exe()
    {
        let other = match tried {
            eframe::Renderer::Glow => "wgpu",
            _ => "glow",
        };
        log::info(&format!("relaunching with the {other} renderer"));
        instance::release();
        if std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env("BUBBLE_RENDERER", other)
            .env(FALLBACK_ENV, "1")
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    let log = log::dir()
        .map(|d| d.join("bubble.log").display().to_string())
        .unwrap_or_default();
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Bubble could not start")
        .set_description(format!(
            "Bubble could not open its window ({e}).\n\n\
             This usually means the graphics driver is missing or too old.\n\n\
             Details are in:\n{log}"
        ))
        .show();
}

/// Whether the saved window place still shows its title bar on a connected screen
/// (a second monitor may have been unplugged since). macOS moves windows back onto a
/// screen itself; on Windows a window could otherwise open where nobody can see it.
#[cfg(windows)]
fn on_a_screen(w: &prefs::WindowPlace) -> bool {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
    // The place is in logical points and the monitor's scale is not known before the
    // window exists, so try the common scale factors: a point on the title bar that
    // lands on any monitor at any of them is good enough.
    [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0].iter().any(|s| {
        let pt = POINT {
            x: ((w.x + 60.0) * s) as i32,
            y: ((w.y + 12.0) * s) as i32,
        };
        // SAFETY: plain value arguments; returns a handle or null.
        !unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONULL) }.is_null()
    })
}

#[cfg(not(windows))]
fn on_a_screen(_w: &prefs::WindowPlace) -> bool {
    true
}
