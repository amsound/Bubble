//! Application menus, described once and shown natively where possible.
//!
//! * macOS: the system menu bar (via `muda`, `NSMenu`).
//! * Windows: the window's menu bar (via `muda`, `HMENU`).
//! * Linux: an in-window egui menu bar (muda needs a GTK window there).
//!
//! The UI builds a [`Menu`] model each frame from current state; the native backend
//! rebuilds the OS menu only when the model changes.

pub use bubble_core::menu::{Cmd, Item, Menu};

/// Runs the model's keyboard shortcuts for the in-window menu bar (the native macOS
/// menu handles its own). Nothing happens while a text field has the keyboard, so
/// Ctrl+C / V / X / Z keep working there.
pub fn egui_shortcuts(ctx: &eframe::egui::Context, menu: &Menu, out: &mut Vec<Cmd>) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    fn walk<'a>(items: &'a [Item], found: &mut Vec<(&'a str, &'a Cmd)>) {
        for it in items {
            match it {
                Item::Sub { items, .. } => walk(items, found),
                Item::Action {
                    cmd,
                    accel: Some(a),
                    enabled: true,
                    ..
                } => found.push((a, cmd)),
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    for top in &menu.tops {
        walk(&top.items, &mut found);
    }
    let mut shortcuts: Vec<_> = found
        .into_iter()
        .filter_map(|(a, cmd)| parse_accel(a).map(|s| (s, cmd)))
        .collect();
    // More modifiers first, so Ctrl+Shift+Z is not taken by Ctrl+Z.
    shortcuts.sort_by_key(|(s, _)| {
        std::cmp::Reverse(
            s.modifiers.shift as u8 + s.modifiers.alt as u8 + s.modifiers.command as u8,
        )
    });
    for (s, cmd) in shortcuts {
        if ctx.input_mut(|i| i.consume_shortcut(&s)) {
            out.push(cmd.clone());
        }
    }
}

/// "CmdOrCtrl+Shift+S" → Ctrl (⌘ on a Mac) + Shift + S.
fn parse_accel(a: &str) -> Option<eframe::egui::KeyboardShortcut> {
    use eframe::egui::{Key, KeyboardShortcut, Modifiers};
    let parts: Vec<&str> = a.split('+').collect();
    let (key, mods) = parts.split_last()?;
    let mut m = Modifiers::NONE;
    for p in mods {
        m |= match *p {
            "CmdOrCtrl" => Modifiers::COMMAND,
            "Shift" => Modifiers::SHIFT,
            "Alt" | "Option" => Modifiers::ALT,
            "Ctrl" => Modifiers::CTRL,
            _ => return None,
        };
    }
    let key = match *key {
        "Comma" => Key::Comma,
        "Period" => Key::Period,
        k => Key::from_name(k)?,
    };
    Some(KeyboardShortcut::new(m, key))
}

/// Draws the model as an egui menu bar (Windows and Linux, or when the native macOS
/// menu fails). It follows the light/dark appearance, which Windows' own menu bar
/// does not.
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

#[cfg(target_os = "macos")]
pub use native::NativeMenu;

#[cfg(target_os = "macos")]
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
    }

    impl NativeMenu {
        pub fn new() -> Self {
            NativeMenu {
                menu: None,
                signature: 0,
                ids: HashMap::new(),
                counter: 0,
                error: None,
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

        /// Rebuilds the OS menu if the model changed.
        pub fn update(&mut self, model: &Menu) {
            let sig = model.signature();
            if sig == self.signature && self.menu.is_some() {
                return;
            }
            self.signature = sig;
            self.ids.clear();
            let root = MudaMenu::new();

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

#[cfg(test)]
mod tests {
    use super::parse_accel;
    use eframe::egui::{Key, Modifiers};

    #[test]
    fn accelerators_parse() {
        let s = parse_accel("CmdOrCtrl+Shift+S").unwrap();
        assert_eq!(s.logical_key, Key::S);
        assert_eq!(s.modifiers, Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!(
            parse_accel("CmdOrCtrl+Comma").unwrap().logical_key,
            Key::Comma
        );
        assert_eq!(
            parse_accel("CmdOrCtrl+Period").unwrap().logical_key,
            Key::Period
        );
        assert!(parse_accel("Hyper+S").is_none());
    }
}
