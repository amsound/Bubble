//! The system colour picker for a cart's "other colour".
//!
//! macOS: the shared Colours panel (NSColorPanel), a floating panel that stays open;
//! its colour is read on each refresh while it is up, so the cart follows it live.
//! Windows: the Colour dialog (ChooseColor) on its own thread, owned by the main
//! window, as the alerts are; its answer arrives on a later refresh.

pub type Rgb = (u8, u8, u8);

/// A colour picked so far, and whether the picker has closed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picked {
    pub rgb: Option<Rgb>,
    pub closed: bool,
}

#[cfg(target_os = "macos")]
pub struct Picker {
    open: bool,
    last: Option<Rgb>,
}

#[cfg(target_os = "macos")]
impl Picker {
    pub fn new() -> Self {
        Picker {
            open: false,
            last: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows the Colours panel set to `rgb`.
    pub fn open(&mut self, rgb: Rgb, _window: &slint::Window) {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSColor, NSColorPanel};
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let panel = NSColorPanel::sharedColorPanel(mtm);
        panel.setShowsAlpha(false);
        let c = |v: u8| v as f64 / 255.0;
        panel.setColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            c(rgb.0),
            c(rgb.1),
            c(rgb.2),
            1.0,
        ));
        panel.makeKeyAndOrderFront(None);
        self.open = true;
        self.last = Some(rgb);
    }

    /// The panel's colour when it changed, and whether the panel was closed.
    pub fn poll(&mut self) -> Option<Picked> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSColorPanel, NSColorSpace};
        if !self.open {
            return None;
        }
        let mtm = MainThreadMarker::new()?;
        let panel = NSColorPanel::sharedColorPanel(mtm);
        let closed = !panel.isVisible();
        if closed {
            self.open = false;
        }
        let rgb = panel
            .color()
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .map(|c| {
                let v = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                (
                    v(c.redComponent()),
                    v(c.greenComponent()),
                    v(c.blueComponent()),
                )
            });
        let changed = rgb.is_some() && rgb != self.last;
        if changed {
            self.last = rgb;
        }
        (changed || closed).then_some(Picked {
            rgb: rgb.filter(|_| changed),
            closed,
        })
    }

    /// Hides the panel (quick edit closed).
    pub fn close(&mut self) {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSColorPanel;
        if self.open
            && let Some(mtm) = MainThreadMarker::new()
            && NSColorPanel::sharedColorPanelExists(mtm)
        {
            NSColorPanel::sharedColorPanel(mtm).orderOut(None);
        }
        self.open = false;
    }
}

#[cfg(not(target_os = "macos"))]
pub struct Picker {
    open: bool,
    tx: std::sync::mpsc::Sender<Option<Rgb>>,
    rx: std::sync::mpsc::Receiver<Option<Rgb>>,
}

#[cfg(not(target_os = "macos"))]
impl Picker {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Picker {
            open: false,
            tx,
            rx,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows the Colour dialog set to `rgb`.
    pub fn open(&mut self, rgb: Rgb, window: &slint::Window) {
        if self.open {
            return;
        }
        self.open = true;
        let tx = self.tx.clone();
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let owner = match window.window_handle().window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
                _ => 0,
            };
            std::thread::Builder::new()
                .name("bubble-colour".into())
                .spawn(move || {
                    let _ = tx.send(choose_colour(rgb, owner));
                })
                .expect("spawn colour dialog thread");
        }
        #[cfg(not(windows))]
        {
            let _ = (rgb, window);
            let _ = tx.send(None);
        }
    }

    pub fn poll(&mut self) -> Option<Picked> {
        let rgb = self.rx.try_recv().ok()?;
        self.open = false;
        Some(Picked { rgb, closed: true })
    }

    /// The Windows dialog is modal to Bubble; it closes itself.
    pub fn close(&mut self) {}
}

/// ChooseColor, with the cart's colour selected and the custom colours kept for the
/// rest of the session.
#[cfg(windows)]
fn choose_colour(rgb: Rgb, owner: isize) -> Option<Rgb> {
    use std::sync::Mutex;
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CC_ANYCOLOR, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW,
    };
    static CUSTOM: Mutex<[u32; 16]> = Mutex::new([0x00ff_ffff; 16]);
    let mut custom = CUSTOM.lock().unwrap_or_else(|e| e.into_inner());
    let mut cc: CHOOSECOLORW = unsafe { std::mem::zeroed() };
    cc.lStructSize = std::mem::size_of::<CHOOSECOLORW>() as u32;
    cc.hwndOwner = owner as _;
    // COLORREF is 0x00BBGGRR.
    cc.rgbResult = rgb.0 as u32 | (rgb.1 as u32) << 8 | (rgb.2 as u32) << 16;
    cc.lpCustColors = custom.as_mut_ptr();
    cc.Flags = CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR;
    if unsafe { ChooseColorW(&mut cc) } == 0 {
        return None;
    }
    let c = cc.rgbResult;
    Some((
        (c & 0xff) as u8,
        (c >> 8 & 0xff) as u8,
        (c >> 16 & 0xff) as u8,
    ))
}
