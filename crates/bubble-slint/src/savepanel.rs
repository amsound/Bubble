//! The system Save panel for New Session, Save As and Import into a new session.
//!
//! macOS: an NSSavePanel sheet on the main window; for a new session it carries the
//! Audio files choice (Keep as imported / Convert to WAV). Windows and elsewhere: the
//! system Save dialog through rfd, on its own thread; the app then asks Audio files
//! in a native alert. Like the alerts, the panel never runs inside a UI callback (see
//! `alerts.rs`); its answer arrives through `done`.

use std::path::PathBuf;

/// What to ask for.
#[derive(Clone, Debug)]
pub struct Request {
    /// "New Session", "Save Session As", "Import into a New Session".
    pub title: String,
    /// The panel's button: Create, Save, Import (a Mac panel; Windows says Save).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub button: String,
    pub name: String,
    pub location: PathBuf,
    /// Ask the Audio files question in the panel (a new session on a Mac; Windows
    /// asks it in an alert afterwards).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub ask_audio: bool,
}

/// The panel's answer: where, and (when it was asked here) the Audio files choice.
#[derive(Clone, Debug)]
pub struct Choice {
    pub path: PathBuf,
    /// `Some(true)` convert to WAV, `Some(false)` keep as imported, `None` not asked
    /// in the panel.
    pub convert: Option<bool>,
}

pub type Done = Box<dyn FnOnce(Option<Choice>) + Send>;

#[cfg(target_os = "macos")]
pub fn show(req: Request, window: &slint::Window, done: Done) {
    use std::cell::Cell;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSModalResponse, NSModalResponseOK, NSPopUpButton, NSSavePanel, NSTextAlignment,
        NSTextField, NSView,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // The main window's view, to hang the sheet on (the pointer crosses to the main
    // queue as a number; it is only used there, on the main thread).
    let view = match window.window_handle().window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::AppKit(h)) => h.ns_view.as_ptr() as usize,
        _ => 0,
    };
    dispatch2::DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return done(None);
        };
        let panel = NSSavePanel::savePanel(mtm);
        panel.setTitle(Some(&NSString::from_str(&req.title)));
        panel.setMessage(Some(&NSString::from_str(&req.title)));
        panel.setPrompt(Some(&NSString::from_str(&req.button)));
        panel.setNameFieldStringValue(&NSString::from_str(&req.name));
        panel.setCanCreateDirectories(true);
        // A session is a folder; Finder tags on it are not Bubble's business.
        panel.setShowsTagField(false);
        let dir = NSURL::fileURLWithPath(&NSString::from_str(&req.location.to_string_lossy()));
        panel.setDirectoryURL(Some(&dir));

        // Audio files: [Keep as imported ▾]
        let popup: Option<Retained<NSPopUpButton>> = req.ask_audio.then(|| {
            let rect = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
            let accessory = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 380.0, 44.0));
            let label = NSTextField::labelWithString(&NSString::from_str("Audio files:"), mtm);
            label.setAlignment(NSTextAlignment::Right);
            label.setFrame(rect(0.0, 13.0, 130.0, 18.0));
            let popup = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                rect(136.0, 8.0, 210.0, 26.0),
                false,
            );
            popup.addItemWithTitle(&NSString::from_str("Keep as imported"));
            popup.addItemWithTitle(&NSString::from_str("Convert to WAV"));
            popup.selectItemAtIndex(0);
            accessory.addSubview(&label);
            accessory.addSubview(&popup);
            panel.setAccessoryView(Some(&accessory));
            popup
        });

        let done = Cell::new(Some(done));
        let p = panel.clone();
        let handler = RcBlock::new(move |r: NSModalResponse| {
            let Some(done) = done.take() else { return };
            if r != NSModalResponseOK {
                return done(None);
            }
            let path = p
                .URL()
                .and_then(|u| u.path())
                .map(|s| PathBuf::from(s.to_string()));
            let convert = popup.as_ref().map(|b| b.indexOfSelectedItem() == 1);
            done(path.map(|path| Choice { path, convert }));
        });
        let window = (view != 0)
            .then(|| unsafe { Retained::retain(view as *mut NSView) })
            .flatten()
            .and_then(|v| v.window());
        match window {
            Some(w) => panel.beginSheetModalForWindow_completionHandler(&w, &handler),
            None => {
                let r = panel.runModal();
                handler.call((r,));
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn show(req: Request, window: &slint::Window, done: Done) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let owner = match window.window_handle().window_handle().map(|h| h.as_raw()) {
        #[cfg(windows)]
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    };
    std::thread::Builder::new()
        .name("bubble-save-panel".into())
        .spawn(move || {
            let mut d = rfd::FileDialog::new()
                .set_title(&req.title)
                .set_directory(&req.location)
                .set_file_name(&req.name);
            if let Some(o) = owner {
                d = d.set_parent(&Owner(o));
            }
            done(d.save_file().map(|path| Choice {
                path,
                convert: None,
            }));
        })
        .expect("spawn save panel thread");
}

/// The main window as the Save dialog's owner, from another thread (Windows).
#[cfg(not(target_os = "macos"))]
struct Owner(isize);

#[cfg(not(target_os = "macos"))]
impl raw_window_handle::HasWindowHandle for Owner {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        #[cfg(windows)]
        {
            let hwnd = std::num::NonZeroIsize::new(self.0)
                .ok_or(raw_window_handle::HandleError::Unavailable)?;
            let raw = raw_window_handle::RawWindowHandle::Win32(
                raw_window_handle::Win32WindowHandle::new(hwnd),
            );
            // SAFETY: the main window outlives the dialog it owns.
            Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
        }
        #[cfg(not(windows))]
        Err(raw_window_handle::HandleError::NotSupported)
    }
}

#[cfg(not(target_os = "macos"))]
impl raw_window_handle::HasDisplayHandle for Owner {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::windows())
    }
}
