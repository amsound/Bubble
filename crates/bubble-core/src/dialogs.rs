//! Non-blocking native file dialogs.
//!
//! Synchronous dialogs run a nested modal event loop inside the UI callback, which
//! re-enters winit on macOS and aborted the app (crash 2026-10-18, panic in
//! `ApplicationDelegate::maybe_queue_event` while the Open panel was up). Instead the
//! dialog future is created on the UI thread (on macOS it is shown as a sheet without
//! blocking) and awaited on a helper thread; the result is picked up on a later frame.

use std::future::Future;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake};
use std::thread;

use uuid::Uuid;

pub const AUDIO_EXTS: &[&str] = &[
    "wav", "wave", "aif", "aiff", "flac", "mp3", "m4a", "aac", "mp4", "ogg", "oga",
];

/// What the chosen file(s) will be used for.
#[derive(Clone, Debug)]
pub enum Purpose {
    Assign {
        page: usize,
        row: u16,
        col: u16,
        many: bool,
    },
    Relink(Uuid),
    /// A Bubble session, or a reference session (.dta) or package (.pkg) to import.
    Open,
    /// Where new sessions are created.
    ChooseFolder,
}

/// File types File ▸ Open accepts.
pub const OPEN_EXTS: &[&str] = &["bubble", "dta", "pkg"];

pub struct FileDialogs {
    queue: Vec<Purpose>,
    /// The next dialog's heading (the message line of a Mac panel).
    title: Option<String>,
    busy: Arc<Mutex<bool>>,
    tx: Sender<(Purpose, Vec<PathBuf>)>,
    rx: Receiver<(Purpose, Vec<PathBuf>)>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl FileDialogs {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = channel();
        FileDialogs {
            queue: Vec::new(),
            title: None,
            busy: Arc::new(Mutex::new(false)),
            tx,
            rx,
            repaint,
        }
    }

    /// Queues a dialog; it is shown at the end of the current frame.
    pub fn request(&mut self, p: Purpose) {
        self.queue.push(p);
    }

    /// Queues a dialog with a heading, e.g. which file to locate.
    pub fn request_titled(&mut self, p: Purpose, title: String) {
        self.title = Some(title);
        self.queue.push(p);
    }

    pub fn is_open(&self) -> bool {
        *self.busy.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Shows the next queued dialog (one at a time). Call from the UI thread.
    /// `parent` owns the dialog, so Windows shows it in front of Bubble rather than
    /// behind a (maximised) main window.
    pub fn launch<W>(&mut self, parent: &W)
    where
        W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + ?Sized,
    {
        if self.queue.is_empty() || self.is_open() {
            self.queue.clear();
            return;
        }
        let purpose = self.queue.remove(0);
        self.queue.clear();
        let title = self.title.take();
        *self.busy.lock().unwrap_or_else(|e| e.into_inner()) = true;

        type Fut = std::pin::Pin<Box<dyn Future<Output = Vec<PathBuf>> + Send>>;
        let fut: Fut = match &purpose {
            Purpose::Assign { many: true, .. } => {
                let f = rfd::AsyncFileDialog::new()
                    .set_parent(parent)
                    .add_filter("Audio", AUDIO_EXTS)
                    .pick_files();
                Box::pin(async move {
                    f.await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|h| h.path().to_path_buf())
                        .collect()
                })
            }
            Purpose::Assign { many: false, .. } | Purpose::Relink(_) => {
                let mut d = rfd::AsyncFileDialog::new()
                    .set_parent(parent)
                    .add_filter("Audio", AUDIO_EXTS);
                if let Some(t) = &title {
                    d = d.set_title(t);
                }
                let f = d.pick_file();
                Box::pin(async move {
                    f.await
                        .map(|h| h.path().to_path_buf())
                        .into_iter()
                        .collect()
                })
            }
            Purpose::Open => {
                let f = rfd::AsyncFileDialog::new()
                    .set_parent(parent)
                    .add_filter("Bubble session or SpotOn session/package", OPEN_EXTS)
                    .pick_file();
                Box::pin(async move {
                    f.await
                        .map(|h| h.path().to_path_buf())
                        .into_iter()
                        .collect()
                })
            }
            Purpose::ChooseFolder => {
                let f = rfd::AsyncFileDialog::new()
                    .set_parent(parent)
                    .set_title("Where to keep sessions")
                    .pick_folder();
                Box::pin(async move {
                    f.await
                        .map(|h| h.path().to_path_buf())
                        .into_iter()
                        .collect()
                })
            }
        };
        let tx = self.tx.clone();
        let busy = self.busy.clone();
        let repaint = self.repaint.clone();
        thread::Builder::new()
            .name("bubble-file-dialog".into())
            .spawn(move || {
                let paths = block_on(fut);
                *busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
                let _ = tx.send((purpose, paths));
                repaint();
            })
            .expect("spawn dialog thread");
    }

    /// Finished dialogs with at least one chosen path.
    pub fn poll(&mut self) -> Vec<(Purpose, Vec<PathBuf>)> {
        self.rx
            .try_iter()
            .filter(|(_, paths)| !paths.is_empty())
            .collect()
    }
}

/// Minimal executor: polls a future on the current thread, parking between wake-ups.
fn block_on<F: Future>(f: F) -> F::Output {
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(thread::current())).into();
    let mut cx = Context::from_waker(&waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}

/// Shows a file in the platform's file manager (Finder, Explorer), or opens its folder.
pub fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if let Some(dir) = path.parent() {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}
