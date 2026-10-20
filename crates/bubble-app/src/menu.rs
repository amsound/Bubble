//! Application menus, described once and shown natively where possible.
//!
//! * macOS: the system menu bar (via `muda`, `NSMenu`).
//! * Windows: the window's menu bar (via `muda`, `HMENU`).
//! * Linux: an in-window egui menu bar (muda needs a GTK window there).
//!
//! The UI builds a [`Menu`] model each frame from current state; the native backend
//! rebuilds the OS menu only when the model changes.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Everything a menu item can ask the app to do.
#[derive(Clone, Debug, PartialEq, Hash)]
pub enum Cmd {
    New,
    Open,
    OpenRecent(usize),
    Save,
    SaveAs,
    Relink,
    ReloadAudio,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PasteAttributes,
    ClearCart,
    AudioSetup,
    PlaySelected,
    ToggleEditMode,
    AddPage,
    RemovePage,
    StopAll,
    StopPage,
    StopAllNow,
    /// Audio ▸ Outputs… window.
    Outputs,
    ReassignOutputs,
    /// Engine sample rate for every output device.
    SampleRate(u32),
    BufferFrames(Option<u32>),
    MidiEnabled,
    MidiAllPorts,
    MidiPort(String),
    RescanMidi,
    MouseToggle,
    MouseLeftRight,
    DuplicatesGanged,
    DuplicatesOverlapped,
    ScopeGlobal,
    ScopePage,
    Limiter,
    ShowLog,
    ShowDiagnostics,
    Zoom(u32),
    /// Global output attenuation preset in dB.
    OutputLevel(i32),
    ProjectSettings,
    RevealSession,
    CopyPage,
    PastePage,
    DuplicateCart,
    PageSettings,
    SpeedBar,
    /// Height of the bottom page/stop bar (0 small, 1 medium, 2 large).
    BarSize(u8),
    /// Interface appearance: follow the system, light or dark.
    Appearance(crate::prefs::Appearance),
    Quit,
}

#[derive(Clone, Debug, Hash)]
pub enum Item {
    Action {
        label: String,
        cmd: Cmd,
        /// Accelerator in muda syntax, e.g. "CmdOrCtrl+S".
        accel: Option<&'static str>,
        checked: Option<bool>,
        enabled: bool,
    },
    Separator,
    Sub {
        label: String,
        items: Vec<Item>,
    },
}

impl Item {
    pub fn action(label: impl Into<String>, cmd: Cmd) -> Item {
        Item::Action {
            label: label.into(),
            cmd,
            accel: None,
            checked: None,
            enabled: true,
        }
    }

    pub fn accel(mut self, a: &'static str) -> Item {
        if let Item::Action { accel, .. } = &mut self {
            *accel = Some(a);
        }
        self
    }

    pub fn check(mut self, on: bool) -> Item {
        if let Item::Action { checked, .. } = &mut self {
            *checked = Some(on);
        }
        self
    }

    pub fn enabled(mut self, on: bool) -> Item {
        if let Item::Action { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    pub fn sub(label: impl Into<String>, items: Vec<Item>) -> Item {
        Item::Sub {
            label: label.into(),
            items,
        }
    }
}

#[derive(Clone, Debug, Hash)]
pub struct Top {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, Hash)]
pub struct Menu {
    pub tops: Vec<Top>,
}

impl Menu {
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    pub fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// Draws the model as an egui menu bar (Linux, or when native menus fail).
pub fn egui_menu_bar(ui: &mut eframe::egui::Ui, menu: &Menu, out: &mut Vec<Cmd>) {
    eframe::egui::MenuBar::new().ui(ui, |ui| {
        for top in &menu.tops {
            ui.menu_button(&top.title, |ui| egui_items(ui, &top.items, out));
        }
    });
}

fn egui_items(ui: &mut eframe::egui::Ui, items: &[Item], out: &mut Vec<Cmd>) {
    for it in items {
        match it {
            Item::Separator => {
                ui.separator();
            }
            Item::Sub { label, items } => {
                ui.menu_button(label, |ui| egui_items(ui, items, out));
            }
            Item::Action {
                label,
                cmd,
                checked,
                enabled,
                accel,
            } => {
                let text = match accel {
                    Some(a) => format!("{label}    {}", a.replace("CmdOrCtrl", "Ctrl")),
                    None => label.clone(),
                };
                let resp = match checked {
                    Some(c) => ui.add_enabled(*enabled, eframe::egui::Button::selectable(*c, text)),
                    None => ui.add_enabled(*enabled, eframe::egui::Button::new(text)),
                };
                if resp.clicked() {
                    out.push(cmd.clone());
                    ui.close();
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::NativeMenu;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use std::collections::HashMap;
    use std::str::FromStr;

    use muda::accelerator::Accelerator;
    use muda::{
        CheckMenuItem, IsMenuItem, Menu as MudaMenu, MenuEvent, MenuItem, PredefinedMenuItem,
        Submenu,
    };

    use super::{Cmd, Item, Menu};

    /// Owns the OS menu and maps item ids back to commands.
    pub struct NativeMenu {
        menu: Option<MudaMenu>,
        signature: u64,
        ids: HashMap<String, Cmd>,
        counter: u64,
        pub error: Option<String>,
        #[cfg(target_os = "windows")]
        hwnd: Option<isize>,
    }

    impl NativeMenu {
        pub fn new() -> Self {
            NativeMenu {
                menu: None,
                signature: 0,
                ids: HashMap::new(),
                counter: 0,
                error: None,
                #[cfg(target_os = "windows")]
                hwnd: None,
            }
        }

        /// Wakes the UI when a menu item is chosen.
        pub fn set_wakeup(f: impl Fn() + Send + Sync + 'static) {
            // The handler replaces the channel; forward to our own queue.
            let tx = event_queue().0.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let _ = tx.send(e);
                f();
            }));
        }

        #[cfg(target_os = "windows")]
        pub fn set_window(&mut self, frame: &eframe::Frame) {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = frame.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    self.hwnd = Some(w.hwnd.get());
                }
            }
        }

        /// Rebuilds the OS menu if the model changed.
        pub fn update(&mut self, model: &Menu) {
            let sig = model.signature();
            if sig == self.signature && self.menu.is_some() {
                return;
            }
            self.signature = sig;
            self.ids.clear();
            let root = MudaMenu::new();

            #[cfg(target_os = "macos")]
            {
                let app = Submenu::new("Bubble", true);
                let _ = app.append_items(&[
                    &PredefinedMenuItem::about(Some("About Bubble"), None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                ]);
                let quit = self.item(&Item::action("Quit Bubble", Cmd::Quit).accel("CmdOrCtrl+Q"));
                let _ = app.append(quit.as_ref());
                let _ = root.append(&app);
            }

            for top in &model.tops {
                let sub = Submenu::new(&top.title, true);
                for it in &top.items {
                    let item = self.item(it);
                    let _ = sub.append(item.as_ref());
                }
                let _ = root.append(&sub);
            }

            #[cfg(target_os = "macos")]
            {
                let window = Submenu::new("Window", true);
                let _ = window.append_items(&[
                    &PredefinedMenuItem::minimize(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::bring_all_to_front(None),
                ]);
                let _ = root.append(&window);
                window.set_as_windows_menu_for_nsapp();
                root.init_for_nsapp();
            }
            #[cfg(target_os = "windows")]
            {
                if let Some(old) = &self.menu {
                    if let Some(h) = self.hwnd {
                        // SAFETY: hwnd comes from the live eframe window.
                        let _ = unsafe { old.remove_for_hwnd(h) };
                    }
                }
                match self.hwnd {
                    // SAFETY: hwnd comes from the live eframe window.
                    Some(h) => {
                        if let Err(e) = unsafe { root.init_for_hwnd(h) } {
                            self.error = Some(e.to_string());
                        }
                    }
                    None => self.error = Some("no window handle yet".into()),
                }
            }
            self.menu = Some(root);
        }

        fn next_id(&mut self, cmd: &Cmd) -> String {
            self.counter += 1;
            let id = format!("bubble-{}", self.counter);
            self.ids.insert(id.clone(), cmd.clone());
            id
        }

        fn item(&mut self, it: &Item) -> Box<dyn IsMenuItem> {
            match it {
                Item::Separator => Box::new(PredefinedMenuItem::separator()),
                Item::Sub { label, items } => {
                    let sub = Submenu::new(label, true);
                    for c in items {
                        let child = self.item(c);
                        let _ = sub.append(child.as_ref());
                    }
                    Box::new(sub)
                }
                Item::Action {
                    label,
                    cmd,
                    accel,
                    checked,
                    enabled,
                } => {
                    let id = self.next_id(cmd);
                    let acc = accel.and_then(|a| Accelerator::from_str(a).ok());
                    match checked {
                        Some(c) => Box::new(CheckMenuItem::with_id(id, label, *enabled, *c, acc)),
                        None => Box::new(MenuItem::with_id(id, label, *enabled, acc)),
                    }
                }
            }
        }

        /// Commands chosen since the last call.
        pub fn poll(&self) -> Vec<Cmd> {
            event_queue()
                .1
                .try_iter()
                .filter_map(|e| self.ids.get(e.id.as_ref()).cloned())
                .collect()
        }
    }

    fn event_queue() -> &'static (
        crossbeam_like::Sender<MenuEvent>,
        crossbeam_like::Receiver<MenuEvent>,
    ) {
        use std::sync::OnceLock;
        static Q: OnceLock<(
            crossbeam_like::Sender<MenuEvent>,
            crossbeam_like::Receiver<MenuEvent>,
        )> = OnceLock::new();
        Q.get_or_init(crossbeam_like::unbounded)
    }

    /// `std::sync::mpsc` with a `Sync` receiver (guarded by a mutex).
    mod crossbeam_like {
        use std::sync::Mutex;
        use std::sync::mpsc;

        pub type Sender<T> = mpsc::SyncSender<T>;
        pub struct Receiver<T>(Mutex<mpsc::Receiver<T>>);

        impl<T> Receiver<T> {
            pub fn try_iter(&self) -> std::vec::IntoIter<T> {
                let rx = self.0.lock().unwrap_or_else(|e| e.into_inner());
                rx.try_iter().collect::<Vec<_>>().into_iter()
            }
        }

        pub fn unbounded<T>() -> (Sender<T>, Receiver<T>) {
            let (tx, rx) = mpsc::sync_channel(1024);
            (tx, Receiver(Mutex::new(rx)))
        }
    }
}
