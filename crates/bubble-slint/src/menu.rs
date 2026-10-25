//! The shared menu model (`bubble_core::menu`) as Slint `MenuBar` data.
//!
//! macOS shows the menu natively and handles its shortcuts. Elsewhere Bubble handles
//! the shortcuts itself ([`Shortcuts`]), as the egui app does, so they never fire while
//! a text field has the keyboard (Ctrl+C/V/X/Z stay with the field); Windows' native
//! menu shows them after a tab.

use bubble_core::menu::{Cmd, Item, Menu};
use slint::{Keys, ModelRc, VecModel};

use crate::keys;
use crate::{MenuEntryData, MenuSlot, MenuTop};

/// The title Slint gives separators inside its menu model (`i_slint_common::
/// MENU_SEPARATOR_PLACEHOLDER_TITLE`). A Slint `MenuBar` cannot choose between an item
/// and a separator per model row, so separators are items with this title.
pub const SEPARATOR: &str = "\u{E001}⸺";

/// Slots per top-level menu in `ui/main.slint` (a run of items, then a submenu).
const SLOTS: usize = 6;

/// Commands that open windows the Slint interface does not have yet (still egui).
pub fn not_here(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::SpeedBar | Cmd::ReassignOutputs | Cmd::ShowLog | Cmd::ShowDiagnostics
    )
}

/// The menus for Slint, and the command behind each entry id.
pub struct Built {
    pub tops: Vec<MenuTop>,
    pub cmds: Vec<Cmd>,
}

struct Slot {
    items: Vec<MenuEntryData>,
    sub_title: String,
    sub: Vec<MenuEntryData>,
}

pub fn build(menu: &Menu) -> Built {
    let mut cmds = Vec::new();
    let tops = menu
        .tops
        .iter()
        .map(|top| {
            let mut slots: Vec<Slot> = vec![Slot::new()];
            for it in &top.items {
                match it {
                    Item::Sub { label, items } => {
                        let sub = items.iter().map(|i| entry(i, &mut cmds, false)).collect();
                        let last = slots.last_mut().expect("a slot");
                        last.sub_title = label.clone();
                        last.sub = sub;
                        slots.push(Slot::new());
                    }
                    _ => {
                        let e = entry(it, &mut cmds, true);
                        slots.last_mut().expect("a slot").items.push(e);
                    }
                }
            }
            if slots.len() > SLOTS {
                bubble_core::log::info(&format!(
                    "menu {}: {} groups, only {SLOTS} shown",
                    top.title,
                    slots.len()
                ));
            }
            MenuTop {
                title: top.title.as_str().into(),
                slots: ModelRc::new(VecModel::from(
                    slots
                        .into_iter()
                        .take(SLOTS)
                        .map(|s| MenuSlot {
                            items: ModelRc::new(VecModel::from(s.items)),
                            sub_title: s.sub_title.into(),
                            sub: ModelRc::new(VecModel::from(s.sub)),
                        })
                        .collect::<Vec<_>>(),
                )),
            }
        })
        .collect();
    Built { tops, cmds }
}

impl Slot {
    fn new() -> Self {
        Slot {
            items: Vec::new(),
            sub_title: String::new(),
            sub: Vec::new(),
        }
    }
}

fn entry(it: &Item, cmds: &mut Vec<Cmd>, top_level: bool) -> MenuEntryData {
    match it {
        Item::Separator => MenuEntryData {
            id: -1,
            title: SEPARATOR.into(),
            enabled: false,
            ..Default::default()
        },
        // Nested submenus are not used by the model; show the name only.
        Item::Sub { label, .. } => MenuEntryData {
            id: -1,
            title: label.as_str().into(),
            enabled: false,
            ..Default::default()
        },
        Item::Action {
            label,
            cmd,
            accel,
            checked,
            enabled,
        } => {
            let id = cmds.len() as i32;
            cmds.push(cmd.clone());
            let missing = not_here(cmd);
            let mut title = if missing {
                format!("{label} (not in this interface yet)")
            } else {
                label.clone()
            };
            let mut shortcut = Keys::default();
            if let Some(a) = accel {
                if cfg!(target_os = "macos") && top_level {
                    shortcut = keys_for(a).unwrap_or_default();
                } else if cfg!(windows) {
                    title = format!("{title}\t{}", keys::accel_label(a));
                }
            }
            MenuEntryData {
                id,
                title: title.into(),
                enabled: *enabled && !missing,
                checkable: checked.is_some(),
                checked: checked.unwrap_or(false),
                shortcut,
            }
        }
    }
}

/// "CmdOrCtrl+Shift+S" as Slint keys (Slint's Control is ⌘ on a Mac).
fn keys_for(accel: &str) -> Option<Keys> {
    let parts: Vec<&str> = accel.split('+').collect();
    let (key, mods) = parts.split_last()?;
    let mut out: Vec<&str> = mods
        .iter()
        .map(|m| match *m {
            "CmdOrCtrl" => "Control",
            "Option" => "Alt",
            m => m,
        })
        .collect();
    let lower = key.to_ascii_lowercase();
    let k: &str = match *key {
        "Comma" => ",",
        "Period" => ".",
        _ => &lower,
    };
    out.push(k);
    match Keys::from_parts(out.iter().copied()) {
        Ok(k) => Some(k),
        Err(e) => {
            bubble_core::log::info(&format!("menu shortcut {accel}: {e}"));
            None
        }
    }
}

/// The menu shortcuts Bubble runs itself (all platforms but macOS).
pub struct Shortcuts {
    list: Vec<(
        bool,
        bool,
        bool,
        slint::winit_030::winit::keyboard::KeyCode,
        Cmd,
    )>,
}

impl Shortcuts {
    pub fn new(menu: &Menu) -> Self {
        fn walk(items: &[Item], out: &mut Vec<(&'static str, Cmd)>) {
            for it in items {
                match it {
                    Item::Sub { items, .. } => walk(items, out),
                    Item::Action {
                        cmd,
                        accel: Some(a),
                        enabled: true,
                        ..
                    } if !not_here(cmd) => out.push((a, cmd.clone())),
                    _ => {}
                }
            }
        }
        let mut found = Vec::new();
        for top in &menu.tops {
            walk(&top.items, &mut found);
        }
        let list = found
            .into_iter()
            .filter_map(|(a, cmd)| keys::parse_accel(a).map(|(c, s, alt, k)| (c, s, alt, k, cmd)))
            .collect();
        Shortcuts { list }
    }

    /// The command for a key press with these modifiers, if any.
    pub fn find(
        &self,
        key: slint::winit_030::winit::keyboard::KeyCode,
        m: slint::winit_030::winit::keyboard::ModifiersState,
    ) -> Option<Cmd> {
        let (c, s, a) = (keys::command(m), m.shift_key(), m.alt_key());
        self.list
            .iter()
            .find(|(cc, ss, aa, k, _)| *k == key && *cc == c && *ss == s && *aa == a)
            .map(|x| x.4.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submenus_get_their_own_slots() {
        let menu = Menu {
            tops: vec![bubble_core::menu::Top {
                title: "Session".into(),
                items: vec![
                    Item::action("A", Cmd::AddPage),
                    Item::Separator,
                    Item::sub("Mouse", vec![Item::action("T", Cmd::MouseToggle)]),
                    Item::sub(
                        "Scope",
                        vec![Item::action("G", Cmd::ScopeGlobal).check(true)],
                    ),
                    Item::action("B", Cmd::StopAll),
                ],
            }],
        };
        let b = build(&menu);
        assert_eq!(b.cmds.len(), 4);
        use slint::Model;
        let slots = &b.tops[0].slots;
        assert_eq!(slots.row_count(), 3);
        let first = slots.row_data(0).unwrap();
        assert_eq!(first.items.row_count(), 2);
        assert_eq!(first.items.row_data(1).unwrap().title, SEPARATOR);
        assert_eq!(first.sub_title, "Mouse");
        let second = slots.row_data(1).unwrap();
        assert_eq!(second.sub_title, "Scope");
        assert!(second.sub.row_data(0).unwrap().checked);
        assert_eq!(
            slots.row_data(2).unwrap().items.row_data(0).unwrap().title,
            "B"
        );
    }

    #[test]
    fn shortcuts_become_slint_keys() {
        assert!(keys_for("CmdOrCtrl+Shift+S").is_some());
        assert!(keys_for("CmdOrCtrl+Period").is_some());
        assert!(keys_for("CmdOrCtrl+Comma").is_some());
    }
}
