//! The main window and the operating system, through Slint's winit accessor
//! (`unstable-winit-030`): its place on screen, the title bar's light/dark theme,
//! bringing it to the front, and where the pointer is during an OS
//! file drag.

use bubble_core::prefs::{Appearance, WindowPlace};
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;
use slint::winit_030::winit;

use crate::MainWindow;

/// The window's current place in logical points,
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
    // macOS: the system setting itself, readable before the window exists and not
    // affected by Bubble's own Light/Dark choice.
    #[cfg(target_os = "macos")]
    {
        let _ = ui;
        use objc2_foundation::{NSString, NSUserDefaults};
        let style = NSUserDefaults::standardUserDefaults()
            .stringForKey(&NSString::from_str("AppleInterfaceStyle"));
        Some(style.is_some_and(|s| s.to_string() == "Dark"))
    }
    #[cfg(not(target_os = "macos"))]
    ui.window()
        .with_winit_window(|w| w.theme())
        .flatten()
        .map(|t| t == winit::window::Theme::Dark)
}

/// The title bar (and on a Mac the app's menus, which macOS draws in the app's
/// appearance) follow Bubble's appearance, not only the system's (W4).
pub fn set_title_bar_theme(ui: &MainWindow, a: Appearance) {
    ui.window().with_winit_window(|w| {
        w.set_theme(match a {
            Appearance::System => None,
            Appearance::Light => Some(winit::window::Theme::Light),
            Appearance::Dark => Some(winit::window::Theme::Dark),
        })
    });
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{
            NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: AppKit's constant appearance names.
        let name = unsafe {
            match a {
                Appearance::System => None,
                Appearance::Light => Some(NSAppearanceNameAqua),
                Appearance::Dark => Some(NSAppearanceNameDarkAqua),
            }
        };
        let appearance = name.and_then(NSAppearance::appearanceNamed);
        NSApplication::sharedApplication(mtm).setAppearance(appearance.as_deref());
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

/// Brings Slint's default macOS app menu in line with the Mac: Quit (⌘Q) closes the
/// window, so it asks about unsaved changes like the close button does; a Settings…
/// item (⌘,; disabled until the settings windows are in this interface); and a Window
/// menu. Slint rebuilds the menu bar when the menus change, so this is checked on
/// every refresh and only acts when its changes are missing.
#[cfg(target_os = "macos")]
pub fn patch_app_menu() {
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(main) = app.mainMenu() else { return };
    let window = NSString::from_str("Window");
    // Slint rebuilds its items when the menus change and our Window menu can end up
    // first, where macOS would show it as the app menu: keep it last.
    if let Some(item) = main.itemWithTitle(&window)
        && main.indexOfItem(&item) != main.numberOfItems() - 1
    {
        main.removeItem(&item);
        main.addItem(&item);
    }
    // The app menu is the one with Quit (⌘Q).
    let has_quit = |m: &NSMenu| {
        (0..m.numberOfItems()).any(|i| {
            m.itemAtIndex(i).is_some_and(|it| {
                it.keyEquivalent().to_string() == "q"
                    && (it.action() == Some(sel!(terminate:))
                        || it.action() == Some(sel!(performClose:)))
            })
        })
    };
    let Some((app_item, app_menu)) = (0..main.numberOfItems())
        .filter_map(|i| main.itemAtIndex(i))
        .filter_map(|it| it.submenu().map(|m| (it, m)))
        .find(|(_, m)| has_quit(m))
    else {
        return;
    };
    // Slint titles it "App"; once the menu bar changes macOS shows that title.
    let name = NSString::from_str("Bubble");
    if app_item.title().to_string() != "Bubble" {
        app_item.setTitle(&name);
        app_menu.setTitle(&name);
    }
    for i in 0..app_menu.numberOfItems() {
        if let Some(item) = app_menu.itemAtIndex(i)
            && item.action() == Some(sel!(terminate:))
        {
            // SAFETY: performClose: is a standard NSWindow action, sent to the key
            // window through the responder chain (nil target).
            unsafe {
                item.setAction(Some(sel!(performClose:)));
                item.setTarget(None);
            }
        }
    }
    let settings = NSString::from_str("Settings…");
    if app_menu.itemWithTitle(&settings).is_none() && app_menu.numberOfItems() >= 2 {
        // An action nothing answers, so the menu shows the item greyed out (an item
        // with no action is enabled).
        // SAFETY: a plain selector; nothing in the responder chain implements it.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &settings,
                Some(sel!(bubbleSettingsNotYetHere:)),
                &NSString::from_str(","),
            )
        };
        app_menu.insertItem_atIndex(&item, 2);
        app_menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), 3);
    }
    if main.itemWithTitle(&window).is_none() {
        let menu = NSMenu::initWithTitle(mtm.alloc(), &window);
        let add = |title: &str, action, key: &str| {
            // SAFETY: standard NSWindow / NSApplication actions with a nil target.
            unsafe {
                menu.addItemWithTitle_action_keyEquivalent(
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            }
        };
        add("Minimize", sel!(performMiniaturize:), "m");
        add("Zoom", sel!(performZoom:), "");
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Bring All to Front", sel!(arrangeInFront:), "");
        let item = NSMenuItem::new(mtm);
        item.setTitle(&window);
        item.setSubmenu(Some(&menu));
        main.addItem(&item);
        app.setWindowsMenu(Some(&menu));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn patch_app_menu() {}

/// Whether this computer has a touch screen (Windows tablets such as the Surface).
#[cfg(windows)]
pub fn has_touch_screen() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_MAXIMUMTOUCHES};
    // SAFETY: a plain query with no pointers.
    unsafe { GetSystemMetrics(SM_MAXIMUMTOUCHES) > 0 }
}

#[cfg(not(windows))]
pub fn has_touch_screen() -> bool {
    false
}
