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
///
/// Windows: a window created maximised is shown, hidden and shown again (winit
/// maximises it on creation with `ShowWindow(SW_MAXIMIZE)`, which shows it, then hides
/// it because it is not visible yet; Slint shows it after its first frame). So a
/// maximised window opens instead filling the screen's work area, not maximised, and
/// `finish_restore` maximises it after the first frame and puts back the size it
/// returns to.
pub fn restore_place(ui: &MainWindow, place: Option<WindowPlace>) {
    let Some(p) = place.filter(WindowPlace::usable) else {
        return;
    };
    let w = ui.window();
    #[cfg(windows)]
    if p.maximized
        && let Some((x, y, width, height)) = work_area_at(&p)
    {
        w.set_position(slint::PhysicalPosition::new(x, y));
        w.set_size(slint::PhysicalSize::new(width, height));
        MAXIMISE_LATER.with(|m| m.set(Some(p)));
        return;
    }
    w.set_size(slint::LogicalSize::new(p.w, p.h));
    w.set_position(slint::LogicalPosition::new(p.x, p.y));
    if p.maximized {
        w.set_maximized(true);
    }
}

#[cfg(windows)]
thread_local! {
    /// The place to maximise into after the first frame (see `restore_place`).
    static MAXIMISE_LATER: std::cell::Cell<Option<WindowPlace>> = const { std::cell::Cell::new(None) };
}

/// Windows: the window still has to be maximised (its place is not worth saving yet).
pub fn restore_pending() -> bool {
    #[cfg(windows)]
    return MAXIMISE_LATER.with(|m| m.get().is_some());
    #[cfg(not(windows))]
    false
}

/// Windows: the work area (screen without the taskbar), in pixels, of the monitor
/// holding the middle of the saved place: (x, y, width, height).
#[cfg(windows)]
fn work_area_at(p: &WindowPlace) -> Option<(i32, i32, u32, u32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    // The saved place is in logical points; the monitor's scale is not known yet, but
    // any point inside the window picks the right monitor at scale 1 to 2.
    let centre = POINT {
        x: (p.x + p.w / 4.0) as i32,
        y: (p.y + p.h / 4.0) as i32,
    };
    // SAFETY: plain queries; MONITORINFO is sized before the call.
    unsafe {
        let mon = MonitorFromPoint(centre, MONITOR_DEFAULTTOPRIMARY);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut info) == 0 {
            return None;
        }
        let r = info.rcWork;
        Some((
            r.left,
            r.top,
            (r.right - r.left).max(1) as u32,
            (r.bottom - r.top).max(1) as u32,
        ))
    }
}

/// Windows: maximises a window `restore_place` opened filling the screen (call once it
/// has drawn), and sets the place un-maximising returns to.
pub fn finish_restore(ui: &MainWindow) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowPlacement, SW_SHOWMAXIMIZED, SetWindowPlacement, WINDOWPLACEMENT,
        };
        let Some(p) = MAXIMISE_LATER.with(|m| m.take()) else {
            return;
        };
        let w = ui.window();
        let Ok(RawWindowHandle::Win32(h)) = w.window_handle().window_handle().map(|h| h.as_raw())
        else {
            w.set_maximized(true);
            return;
        };
        let hwnd = h.hwnd.get() as _;
        let s = w.scale_factor();
        // The saved size is the inside of the window; the restore rectangle is its
        // outside, so add the frame (title bar and borders) as it is now, unmaximised.
        let (fw, fh) = w
            .with_winit_window(|ww| {
                let (o, i) = (ww.outer_size(), ww.inner_size());
                (
                    o.width.saturating_sub(i.width) as i32,
                    o.height.saturating_sub(i.height) as i32,
                )
            })
            .unwrap_or((0, 0));
        // SAFETY: the window's own handle; WINDOWPLACEMENT is sized before use.
        unsafe {
            let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
            wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if GetWindowPlacement(hwnd, &mut wp) == 0 {
                w.set_maximized(true);
                return;
            }
            // The restore rectangle is in workspace coordinates; with the taskbar at
            // the bottom or right they are screen coordinates, and Windows moves a
            // window that would sit under a taskbar on the top or left itself.
            wp.rcNormalPosition.left = (p.x * s) as i32;
            wp.rcNormalPosition.top = (p.y * s) as i32;
            wp.rcNormalPosition.right = (p.x * s) as i32 + (p.w * s) as i32 + fw;
            wp.rcNormalPosition.bottom = (p.y * s) as i32 + (p.h * s) as i32 + fh;
            wp.showCmd = SW_SHOWMAXIMIZED as u32;
            // Without Windows' maximise animation (a half blink as the frame and the
            // rounded corners change), on for this moment only.
            set_transitions(hwnd as isize, false);
            if SetWindowPlacement(hwnd, &wp) == 0 {
                w.set_maximized(true);
            }
            let h = hwnd as isize;
            slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
                set_transitions(h, true)
            });
        }
    }
    #[cfg(not(windows))]
    let _ = ui;
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

thread_local! {
    /// The last pop-up menu: where it opened and when it closed.
    static LAST_MENU: std::cell::Cell<Option<(f32, f32, std::time::Instant)>> =
        const { std::cell::Cell::new(None) };
}

/// Opens a system menu at (x, y); `open` returns when the menu has closed. A mouse
/// click on the button that opened it closes the menu and then reaches the button,
/// which would open it again (a tap does not): so a request from the same place just
/// after it closed is that click, and the menu stays closed.
pub fn run_menu(x: f32, y: f32, open: impl FnOnce()) {
    let same_click = LAST_MENU.get().is_some_and(|(lx, ly, closed)| {
        (lx - x).abs() < 1.0
            && (ly - y).abs() < 1.0
            && closed.elapsed() < std::time::Duration::from_millis(500)
    });
    LAST_MENU.set(None);
    if same_click {
        return;
    }
    open();
    LAST_MENU.set(Some((x, y, std::time::Instant::now())));
}

/// A tool window just shown (Cart Setup, the settings windows): rounded corners, the
/// icon in its title bar, kept in front of the main window. Done again a moment later:
/// on Windows the window may not exist yet straight after `show`, and the icon set
/// then was lost (the main window sets its own after its first frame).
pub fn finish_tool<T: slint::ComponentHandle + 'static>(win: &T, ui: &MainWindow) {
    fn apply(w: &slint::Window, p: &slint::Window) {
        round_corners(w);
        set_icon(w);
        keep_above(w, p);
    }
    apply(win.window(), ui.window());
    let (w, p) = (win.as_weak(), ui.as_weak());
    slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
        if let (Some(w), Some(p)) = (w.upgrade(), p.upgrade()) {
            apply(w.window(), p.window());
        }
    });
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

/// Places a tool window (`w` × `h` logical points, before it is shown) in the middle of
/// the main window, and inside it when it fits: never under the Windows taskbar or
/// off the bottom of the screen when the main window is maximised.
pub fn place_over(child: &slint::Window, parent: &slint::Window, w: f32, h: f32) {
    let s = parent.scale_factor();
    let pos = parent.position();
    let size = parent.size();
    let (px, py) = (pos.x as f32 / s, pos.y as f32 / s);
    let (pw, ph) = (size.width as f32 / s, size.height as f32 / s);
    // The tool window's title bar is outside `h`.
    const TITLE: f32 = 32.0;
    let x = px + ((pw - w) / 2.0).max(0.0);
    let y = py + ((ph - h - TITLE) / 2.0).max(0.0);
    child.set_position(slint::LogicalPosition::new(x, y));
}

/// Windows: turns the window's open, close, minimise and maximise animations on or off.
#[cfg(windows)]
fn set_transitions(hwnd: isize, on: bool) {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute,
    };
    let disabled: i32 = (!on).into();
    // SAFETY: the window's own handle and a BOOL, as the attribute takes.
    unsafe {
        DwmSetWindowAttribute(
            hwnd as _,
            DWMWA_TRANSITIONS_FORCEDISABLED as u32,
            &disabled as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&disabled) as u32,
        );
    }
}

/// Puts Bubble's logo in the window's title bar and the taskbar (Windows). Slint only
/// passes a window icon on when it changes, judged by a cache key, and an icon made
/// from pixels (ours) has none, so it never arrived: set it on the window directly.
pub fn set_icon(w: &slint::Window) {
    #[cfg(windows)]
    {
        let rgba: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba"));
        if let Ok(icon) = winit::window::Icon::from_rgba(rgba.to_vec(), 256, 256) {
            w.with_winit_window(|ww| ww.set_window_icon(Some(icon)));
        }
    }
    #[cfg(not(windows))]
    let _ = w;
}

/// The system's accent colour as (base, lighter, deeper) RGB, or `None` to keep
/// Bubble's own. Windows: its accent palette (the shades Windows 11's own controls use
/// in dark and light appearance). macOS: the accent colour in all three.
pub fn system_accent() -> Option<[[u8; 3]; 3]> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW,
        };
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent");
        let value = wide("AccentPalette");
        // Eight RGBA colours: light 3, light 2, light 1, the accent, dark 1, 2, 3, unused.
        let mut buf = [0u8; 32];
        let mut len = buf.len() as u32;
        // SAFETY: a plain registry read into a buffer of the size given.
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_BINARY,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        } == 0;
        if !ok || len < 32 {
            return None;
        }
        let at = |i: usize| [buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2]];
        Some([at(3), at(1), at(4)])
    }
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSColor, NSColorSpace};
        let c =
            NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        let v = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        let rgb = [
            v(c.redComponent()),
            v(c.greenComponent()),
            v(c.blueComponent()),
        ];
        Some([rgb, rgb, rgb])
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    None
}

/// Windows 11: asks for the system's rounded window corners (Windows 10 has none and
/// ignores it). Call once the window exists.
pub fn round_corners(w: &slint::Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
        };
        if let Ok(RawWindowHandle::Win32(h)) = w.window_handle().window_handle().map(|h| h.as_raw())
        {
            let pref = DWMWCP_ROUND;
            // SAFETY: the window's own handle and a 4-byte value, as the attribute takes.
            unsafe {
                DwmSetWindowAttribute(
                    h.hwnd.get() as _,
                    DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                    &pref as *const _ as *const std::ffi::c_void,
                    std::mem::size_of_val(&pref) as u32,
                );
            }
        }
    }
    #[cfg(not(windows))]
    let _ = w;
}

/// Windows: whether the right mouse button is down now. Windows ends a context menu on
/// a right-click elsewhere and keeps that click from the window under it.
/// Also true when it was pressed and already let go since the last call (a quick
/// right-click), so call it once before the menu opens to start afresh.
pub fn right_button_down() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_RBUTTON};
        // SAFETY: a plain query of the button's state. The high bit: down now; the low
        // bit: pressed since the last query.
        unsafe { GetAsyncKeyState(VK_RBUTTON as i32) as u16 & 0x8001 != 0 }
    }
    #[cfg(not(windows))]
    false
}

/// Ends a press the window is still tracking, before something modal (a menu, the
/// Colour dialog) takes the mouse: its release would never arrive, and the next click
/// would go to whatever was pressed. Released off the window, so it clicks nothing.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn end_press(w: &slint::Window) {
    for button in [
        slint::platform::PointerEventButton::Left,
        slint::platform::PointerEventButton::Right,
    ] {
        w.dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(-10_000.0, -10_000.0),
            button,
        });
    }
}

/// The window icon, rendered from `assets/logo.svg` by the build script.
pub fn icon() -> slint::Image {
    let rgba: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba"));
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba, 256, 256);
    slint::Image::from_rgba8(buf)
}

/// Brings Slint's default macOS app menu in line with the Mac: Quit (⌘Q) closes the
/// window, so it asks about unsaved changes like the close button does; a Settings…
/// item for this computer's settings (`on_settings`; ⌘, is Session Settings, in the
/// Session menu); and a Window menu. Slint rebuilds the menu bar when the menus change, so this is checked on
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
        // SAFETY: openSettings: is implemented by `SettingsTarget`, kept alive below
        // (a menu item does not retain its target).
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &settings,
                Some(sel!(openSettings:)),
                &NSString::from_str(""),
            )
        };
        let target = SETTINGS_TARGET.with(|t| {
            t.borrow_mut()
                .get_or_insert_with(|| SettingsTarget::new(mtm))
                .clone()
        });
        // SAFETY: the target answers the item's action.
        unsafe { item.setTarget(Some(&target)) };
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

#[cfg(target_os = "macos")]
objc2::define_class!(
    /// Answers Bubble ▸ Settings… by running the handler `on_settings` set.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[name = "BubbleSettingsTarget"]
    struct SettingsTarget;

    impl SettingsTarget {
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            ON_SETTINGS.with(|f| {
                if let Some(f) = f.borrow().as_ref() {
                    f();
                }
            });
        }
    }
);

#[cfg(target_os = "macos")]
impl SettingsTarget {
    fn new(mtm: objc2::MainThreadMarker) -> objc2::rc::Retained<Self> {
        use objc2::MainThreadOnly;
        // SAFETY: NSObject's init, on a freshly allocated object.
        unsafe { objc2::msg_send![Self::alloc(mtm), init] }
    }
}

#[cfg(target_os = "macos")]
thread_local! {
    static SETTINGS_TARGET: std::cell::RefCell<Option<objc2::rc::Retained<SettingsTarget>>> =
        const { std::cell::RefCell::new(None) };
}

thread_local! {
    static ON_SETTINGS: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// What Bubble ▸ Settings… does (macOS; elsewhere it is File ▸ Settings…).
pub fn on_settings(f: impl Fn() + 'static) {
    ON_SETTINGS.with(|s| *s.borrow_mut() = Some(Box::new(f)));
}

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

/// Runs `f` on the UI thread once the current event handler has returned. Native
/// menus and alerts run their own event loop; started inside a Slint timer or a winit
/// handler, that loop re-enters them (a panic on macOS). On a Mac the main dispatch
/// queue runs it outside both; elsewhere Slint's own queue.
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(f);
    #[cfg(not(target_os = "macos"))]
    let _ = slint::invoke_from_event_loop(f);
}

#[cfg(target_os = "macos")]
thread_local! {
    static MAIN_MENU: std::cell::RefCell<Option<objc2::rc::Retained<objc2_app_kit::NSMenu>>> =
        const { std::cell::RefCell::new(None) };
}

/// A Mac has one menu bar, and Slint gives a window with no menu of its own an empty
/// one when it becomes key. So a tool window keeps the main window's: remember it
/// while the main window is key, and put it back when the tool window becomes key.
pub fn remember_menu() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let menu = objc2_app_kit::NSApplication::sharedApplication(mtm).mainMenu();
        MAIN_MENU.with(|m| *m.borrow_mut() = menu);
    }
}

/// See `remember_menu`. Runs after Slint has swapped in its own.
pub fn restore_menu() {
    #[cfg(target_os = "macos")]
    on_main(|| {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        MAIN_MENU.with(|m| {
            if let Some(menu) = m.borrow().as_ref() {
                objc2_app_kit::NSApplication::sharedApplication(mtm).setMainMenu(Some(menu));
            }
        });
    });
}

/// Whether the window has the keyboard (is the key window).
pub fn is_key(w: &slint::Window) -> bool {
    w.with_winit_window(|w| w.has_focus()).unwrap_or(false)
}

/// Keeps a tool window (Cart Setup) in front of the main window until it is closed.
/// macOS: the floating level, hidden while Bubble is in the background, as inspector
/// windows are. Windows: owned by the main window, which it then always stays above.
pub fn keep_above(child: &slint::Window, parent: &slint::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let raw = |w: &slint::Window| w.window_handle().window_handle().map(|h| h.as_raw()).ok();
    #[cfg(target_os = "macos")]
    {
        let _ = parent;
        use objc2::rc::Retained;
        use objc2_app_kit::{NSFloatingWindowLevel, NSView};
        if let Some(RawWindowHandle::AppKit(h)) = raw(child)
            && let Some(view) = unsafe { Retained::retain(h.ns_view.as_ptr() as *mut NSView) }
            && let Some(win) = view.window()
        {
            win.setLevel(NSFloatingWindowLevel);
            win.setHidesOnDeactivate(true);
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GWLP_HWNDPARENT, SetWindowLongPtrW};
        if let (Some(RawWindowHandle::Win32(c)), Some(RawWindowHandle::Win32(p))) =
            (raw(child), raw(parent))
        {
            unsafe { SetWindowLongPtrW(c.hwnd.get() as _, GWLP_HWNDPARENT, p.hwnd.get()) };
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (child, parent, raw);
}
