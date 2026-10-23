//! Native alerts: NSAlert on macOS, TaskDialog on Windows (the plain message box when
//! TaskDialog is not available), so Bubble's questions look like the system's own.
//!
//! Alerts never block inside the UI callback that asked for one: a nested modal loop
//! there re-enters winit on macOS (the 2026-10-18 crash, see `bubble_core::dialogs`).
//! On macOS the alert runs from the main dispatch queue, outside winit's event
//! handling; on Windows it runs on its own thread, owned by the main window. The
//! answer is picked up on a later frame with [`Alerts::poll`].

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// One alert. Button text is the platform's own wording (title case on a Mac,
/// sentence case on Windows).
#[derive(Clone, Debug, Default)]
pub struct Alert {
    /// The question or what happened, in bold.
    pub title: String,
    /// What it means, under the title (may be empty).
    pub info: String,
    /// The first is the default (Return). One titled "Cancel" also answers Esc.
    pub buttons: Vec<String>,
    /// This button deletes or discards (red on a Mac).
    #[cfg_attr(windows, allow(dead_code))]
    pub destructive: Option<usize>,
    /// Something went wrong (the caution badge / error icon).
    pub error: bool,
    /// Windows: the buttons other than Cancel are command links (a choice of actions).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub links: bool,
}

/// The alert being shown, if any, and its answer when it comes.
pub struct Alerts {
    tx: Sender<usize>,
    rx: Receiver<usize>,
    open: bool,
}

impl Alerts {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Alerts {
            tx,
            rx,
            open: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows `alert` over `window`; `wake` is called when it is answered.
    pub fn show(
        &mut self,
        alert: Alert,
        window: &slint::Window,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) {
        self.open = true;
        let tx = self.tx.clone();
        let done = move |i: usize| {
            let _ = tx.send(i);
            wake();
        };
        platform::show(alert, window, Box::new(done));
    }

    /// The button chosen in the alert that just closed.
    pub fn poll(&mut self) -> Option<usize> {
        let i = self.rx.try_recv().ok()?;
        self.open = false;
        Some(i)
    }
}

type Done = Box<dyn FnOnce(usize) + Send>;

#[cfg(target_os = "macos")]
mod platform {
    use super::{Alert, Done};
    use objc2::rc::Retained;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSImage};
    use objc2_foundation::{NSData, NSString};

    /// Bubble's icon, for alerts shown before the app is bundled (a bundled app's
    /// alerts use its own icon anyway).
    static ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png"));

    pub fn show(alert: Alert, _window: &slint::Window, done: Done) {
        dispatch2::DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return done(alert.buttons.len().saturating_sub(1));
            };
            let a = NSAlert::new(mtm);
            a.setMessageText(&NSString::from_str(&alert.title));
            a.setInformativeText(&NSString::from_str(&alert.info));
            a.setAlertStyle(if alert.error {
                NSAlertStyle::Critical
            } else {
                NSAlertStyle::Warning
            });
            let data = NSData::with_bytes(ICON);
            let icon: Option<Retained<NSImage>> = NSImage::initWithData(NSImage::alloc(), &data);
            unsafe { a.setIcon(icon.as_deref()) };
            for (i, b) in alert.buttons.iter().enumerate() {
                let button = a.addButtonWithTitle(&NSString::from_str(b));
                if alert.destructive == Some(i) {
                    button.setHasDestructiveAction(true);
                }
            }
            let r = a.runModal();
            let i = (r - NSAlertFirstButtonReturn).max(0) as usize;
            done(i.min(alert.buttons.len().saturating_sub(1)));
        });
    }
}

#[cfg(windows)]
mod platform {
    use super::{Alert, Done};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::UI::Controls::{
        TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TD_ERROR_ICON, TD_WARNING_ICON, TDCBF_CANCEL_BUTTON,
        TDF_ALLOW_DIALOG_CANCELLATION, TDF_POSITION_RELATIVE_TO_WINDOW, TDF_USE_COMMAND_LINKS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDCANCEL, IDNO, IDOK, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_OKCANCEL,
        MB_YESNOCANCEL, MessageBoxW,
    };

    type TaskDialogIndirect =
        unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut i32) -> i32;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn show(alert: Alert, window: &slint::Window, done: Done) {
        let hwnd = match window.window_handle().window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        std::thread::Builder::new()
            .name("bubble-alert".into())
            .spawn(move || done(run(&alert, hwnd as HWND)))
            .expect("spawn alert thread");
    }

    /// TaskDialog lives in Common Controls 6, which the .exe's manifest asks for. Without
    /// it, the plain message box (its buttons named in the text).
    fn run(alert: &Alert, parent: HWND) -> usize {
        let cancel = alert.buttons.iter().position(|b| b == "Cancel");
        let task: Option<TaskDialogIndirect> = unsafe {
            let lib = LoadLibraryW(wide("comctl32.dll").as_ptr());
            if lib.is_null() {
                None
            } else {
                GetProcAddress(lib, c"TaskDialogIndirect".as_ptr().cast())
                    .map(|f| std::mem::transmute::<_, TaskDialogIndirect>(f))
            }
        };
        let Some(task) = task else {
            return message_box(alert, parent, cancel);
        };
        let texts: Vec<Vec<u16>> = alert.buttons.iter().map(|b| wide(b)).collect();
        let buttons: Vec<TASKDIALOG_BUTTON> = texts
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != cancel)
            .map(|(i, t)| TASKDIALOG_BUTTON {
                nButtonID: 100 + i as i32,
                pszButtonText: t.as_ptr(),
            })
            .collect();
        let title = wide("Bubble");
        let main = wide(&alert.title);
        let content = wide(&alert.info);
        let mut c: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
        c.cbSize = std::mem::size_of::<TASKDIALOGCONFIG>() as u32;
        c.hwndParent = parent;
        c.dwFlags = TDF_POSITION_RELATIVE_TO_WINDOW
            | if alert.links {
                TDF_USE_COMMAND_LINKS
            } else {
                0
            }
            | if cancel.is_some() {
                TDF_ALLOW_DIALOG_CANCELLATION
            } else {
                0
            };
        c.dwCommonButtons = if cancel.is_some() {
            TDCBF_CANCEL_BUTTON
        } else {
            0
        };
        c.pszWindowTitle = title.as_ptr();
        c.Anonymous1.pszMainIcon = if alert.error {
            TD_ERROR_ICON
        } else {
            TD_WARNING_ICON
        };
        c.pszMainInstruction = main.as_ptr();
        if !alert.info.is_empty() {
            c.pszContent = content.as_ptr();
        }
        c.cButtons = buttons.len() as u32;
        c.pButtons = buttons.as_ptr();
        c.nDefaultButton = if Some(0) == cancel { IDCANCEL } else { 100 };
        let mut pressed = 0i32;
        let ok = unsafe { task(&c, &mut pressed, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok < 0 {
            return message_box(alert, parent, cancel);
        }
        if pressed == IDCANCEL {
            cancel.unwrap_or(alert.buttons.len().saturating_sub(1))
        } else {
            (pressed - 100).clamp(0, alert.buttons.len() as i32 - 1) as usize
        }
    }

    fn message_box(alert: &Alert, parent: HWND, cancel: Option<usize>) -> usize {
        let n = alert.buttons.len();
        let names = alert.buttons.join(" · ");
        let text = wide(&format!("{}\n\n{}\n\n({names})", alert.title, alert.info));
        let caption = wide("Bubble");
        // Cancel is the box's Cancel; the others take OK, or Yes then No, in order.
        let (kind, mut free): (u32, Vec<i32>) = match (n, cancel.is_some()) {
            (0 | 1, _) => (MB_OK, vec![IDOK]),
            (2, true) => (MB_OKCANCEL, vec![IDOK]),
            (2, false) => (MB_OKCANCEL, vec![IDOK, IDCANCEL]),
            _ => (MB_YESNOCANCEL, vec![IDYES, IDNO, IDCANCEL]),
        };
        free.reverse();
        let ids: Vec<i32> = (0..n)
            .map(|i| {
                if Some(i) == cancel {
                    IDCANCEL
                } else {
                    free.pop().unwrap_or(0)
                }
            })
            .collect();
        let icon = if alert.error {
            MB_ICONERROR
        } else {
            MB_ICONWARNING
        };
        let r = unsafe { MessageBoxW(parent, text.as_ptr(), caption.as_ptr(), kind | icon) };
        ids.iter()
            .position(|&id| id == r)
            .or(cancel)
            .unwrap_or(n.saturating_sub(1))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{Alert, Done};

    /// Elsewhere rfd's message box, on its own thread.
    pub fn show(alert: Alert, _window: &slint::Window, done: Done) {
        std::thread::spawn(move || {
            let n = alert.buttons.len();
            let r = rfd::MessageDialog::new()
                .set_title(&alert.title)
                .set_description(&alert.info)
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show();
            done(if r == rfd::MessageDialogResult::Ok {
                0
            } else {
                n.saturating_sub(1)
            });
        });
    }
}
