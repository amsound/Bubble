//! Bubble — cart player, Slint interface (main window; D-39).
//!
//! `bubble-slint [session.bubble | file.dta | file.pkg]`
//!
//! Renderer: femtovg (OpenGL) by default, the software renderer as a fallback.
//! `BUBBLE_RENDERER=software` (or `femtovg`) chooses one; if the window cannot be
//! opened, Bubble relaunches itself once with the other.

// No console window behind the app on Windows (crash reports go to a file).
#![cfg_attr(windows, windows_subsystem = "windows")]

use bubble_core::{instance, log};

mod alerts;
mod app;
mod cartsetup;
mod colour;
mod fonts;
mod keys;
mod menu;
mod model;
mod savepanel;
mod settings;
mod window;

slint::include_modules!();

/// Set on a relaunch with the other renderer, so a second failure is reported
/// instead of relaunching again.
const FALLBACK_ENV: &str = "BUBBLE_RENDERER_FALLBACK";

fn main() {
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
        return;
    }

    no_power_throttling();
    let ui_font = fonts::prepare();
    let renderer = renderer();
    log::info(&format!(
        "starting the Slint interface with the {renderer} renderer"
    ));
    let result = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.into())
        .select()
        .and_then(|()| app::run(renderer, ui_font));
    match result {
        Ok(()) => log::info("exited normally"),
        Err(e) => {
            log::info(&format!("could not start with {renderer}: {e}"));
            failed_to_start(renderer, &e);
        }
    }
}

/// Windows 10 and 11 slow down the threads of a program they think is in the
/// background to save power (EcoQoS), and may stop honouring its timer requests. During
/// a show Bubble often sits behind another window while it plays, and its disk reader
/// and refresh must keep their pace: opt out for the whole process.
#[cfg(windows)]
fn no_power_throttling() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        // Control both, and turn both off (state 0).
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    // SAFETY: the current process's pseudo-handle and a correctly sized struct.
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    log::info(if ok != 0 {
        "power throttling off for Bubble"
    } else {
        "could not turn power throttling off (Windows older than 10 1709?)"
    });
}

#[cfg(not(windows))]
fn no_power_throttling() {}

/// femtovg (OpenGL) unless `BUBBLE_RENDERER=software`. On a Windows Surface femtovg
/// performed best; the software renderer needs no GPU at all.
fn renderer() -> &'static str {
    match std::env::var("BUBBLE_RENDERER").as_deref() {
        Ok("software" | "sw") => "software",
        Ok("skia") if cfg!(target_os = "macos") => "skia",
        _ => "femtovg",
    }
}

/// The window could not be created (e.g. no usable OpenGL driver). The event loop
/// cannot be started twice in one process, so Bubble relaunches itself once with the
/// other renderer; if that fails too, it says so instead of vanishing.
fn failed_to_start(tried: &str, e: &slint::PlatformError) {
    if std::env::var_os(FALLBACK_ENV).is_none()
        && let Ok(exe) = std::env::current_exe()
    {
        let other = if tried == "software" {
            "femtovg"
        } else {
            "software"
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
