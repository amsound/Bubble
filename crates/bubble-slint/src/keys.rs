//! Hot keys from physical key positions, named as the egui app named them (and as
//! sessions and the SpotOn import store them): "KeyA", "Digit1", "F1", "Up",
//! "Escape", "Space", "OpenBracket", … Numpad digits count as the digit keys.

use slint::winit_030::winit::keyboard::{KeyCode, ModifiersState};

/// The binding name of a physical key, or `None` for keys Bubble does not bind.
pub fn code_name(k: KeyCode) -> Option<String> {
    use KeyCode as K;
    let s = match k {
        K::KeyA => "KeyA",
        K::KeyB => "KeyB",
        K::KeyC => "KeyC",
        K::KeyD => "KeyD",
        K::KeyE => "KeyE",
        K::KeyF => "KeyF",
        K::KeyG => "KeyG",
        K::KeyH => "KeyH",
        K::KeyI => "KeyI",
        K::KeyJ => "KeyJ",
        K::KeyK => "KeyK",
        K::KeyL => "KeyL",
        K::KeyM => "KeyM",
        K::KeyN => "KeyN",
        K::KeyO => "KeyO",
        K::KeyP => "KeyP",
        K::KeyQ => "KeyQ",
        K::KeyR => "KeyR",
        K::KeyS => "KeyS",
        K::KeyT => "KeyT",
        K::KeyU => "KeyU",
        K::KeyV => "KeyV",
        K::KeyW => "KeyW",
        K::KeyX => "KeyX",
        K::KeyY => "KeyY",
        K::KeyZ => "KeyZ",
        K::Digit0 | K::Numpad0 => "Digit0",
        K::Digit1 | K::Numpad1 => "Digit1",
        K::Digit2 | K::Numpad2 => "Digit2",
        K::Digit3 | K::Numpad3 => "Digit3",
        K::Digit4 | K::Numpad4 => "Digit4",
        K::Digit5 | K::Numpad5 => "Digit5",
        K::Digit6 | K::Numpad6 => "Digit6",
        K::Digit7 | K::Numpad7 => "Digit7",
        K::Digit8 | K::Numpad8 => "Digit8",
        K::Digit9 | K::Numpad9 => "Digit9",
        K::F1 => "F1",
        K::F2 => "F2",
        K::F3 => "F3",
        K::F4 => "F4",
        K::F5 => "F5",
        K::F6 => "F6",
        K::F7 => "F7",
        K::F8 => "F8",
        K::F9 => "F9",
        K::F10 => "F10",
        K::F11 => "F11",
        K::F12 => "F12",
        K::F13 => "F13",
        K::F14 => "F14",
        K::F15 => "F15",
        K::F16 => "F16",
        K::F17 => "F17",
        K::F18 => "F18",
        K::F19 => "F19",
        K::F20 => "F20",
        K::ArrowUp => "Up",
        K::ArrowDown => "Down",
        K::ArrowLeft => "Left",
        K::ArrowRight => "Right",
        K::Escape => "Escape",
        K::Tab => "Tab",
        K::Backspace => "Backspace",
        K::Enter | K::NumpadEnter => "Enter",
        K::Space => "Space",
        K::Insert => "Insert",
        K::Delete => "Delete",
        K::Home => "Home",
        K::End => "End",
        K::PageUp => "PageUp",
        K::PageDown => "PageDown",
        K::Minus | K::NumpadSubtract => "Minus",
        K::Equal | K::NumpadEqual => "Equals",
        K::NumpadAdd => "Plus",
        K::Comma | K::NumpadComma => "Comma",
        K::Period | K::NumpadDecimal => "Period",
        K::Semicolon => "Semicolon",
        K::Slash | K::NumpadDivide => "Slash",
        K::Backslash => "Backslash",
        K::BracketLeft => "OpenBracket",
        K::BracketRight => "CloseBracket",
        K::Backquote => "Backtick",
        K::Quote => "Quote",
        _ => return None,
    };
    Some(s.to_string())
}

/// Sorted modifier names, as the egui app reported them: "Alt", "Ctrl", "Meta"
/// (the ⌘ key, on a Mac only) and "Shift".
pub fn modifier_names(m: ModifiersState) -> Vec<String> {
    let mut v = Vec::new();
    if m.alt_key() {
        v.push("Alt".to_string());
    }
    if m.control_key() {
        v.push("Ctrl".to_string());
    }
    if cfg!(target_os = "macos") && m.super_key() {
        v.push("Meta".to_string());
    }
    if m.shift_key() {
        v.push("Shift".to_string());
    }
    v.sort();
    v
}

/// The platform's command key is held (⌘ on a Mac, Ctrl elsewhere).
pub fn command(m: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        m.super_key()
    } else {
        m.control_key()
    }
}

/// Keys whose ⌘/Ctrl combinations belong to the menus, never to hot keys.
pub fn menu_key(k: KeyCode) -> bool {
    use KeyCode as K;
    matches!(
        k,
        K::KeyS
            | K::KeyO
            | K::KeyN
            | K::KeyZ
            | K::KeyX
            | K::KeyC
            | K::KeyV
            | K::KeyE
            | K::KeyI
            | K::KeyD
            | K::KeyL
            | K::KeyQ
            | K::KeyW
            | K::Comma
            | K::Period
    )
}

/// A menu accelerator ("CmdOrCtrl+Shift+S") as (command, shift, alt, key).
pub fn parse_accel(a: &str) -> Option<(bool, bool, bool, KeyCode)> {
    let parts: Vec<&str> = a.split('+').collect();
    let (key, mods) = parts.split_last()?;
    let (mut cmd, mut shift, mut alt) = (false, false, false);
    for p in mods {
        match *p {
            "CmdOrCtrl" => cmd = true,
            "Shift" => shift = true,
            "Alt" | "Option" => alt = true,
            _ => return None,
        }
    }
    use KeyCode as K;
    let key = match *key {
        "Comma" => K::Comma,
        "Period" => K::Period,
        k if k.len() == 1 => match k.as_bytes()[0].to_ascii_uppercase() {
            b'A' => K::KeyA,
            b'B' => K::KeyB,
            b'C' => K::KeyC,
            b'D' => K::KeyD,
            b'E' => K::KeyE,
            b'F' => K::KeyF,
            b'G' => K::KeyG,
            b'H' => K::KeyH,
            b'I' => K::KeyI,
            b'J' => K::KeyJ,
            b'K' => K::KeyK,
            b'L' => K::KeyL,
            b'M' => K::KeyM,
            b'N' => K::KeyN,
            b'O' => K::KeyO,
            b'P' => K::KeyP,
            b'Q' => K::KeyQ,
            b'R' => K::KeyR,
            b'S' => K::KeyS,
            b'T' => K::KeyT,
            b'U' => K::KeyU,
            b'V' => K::KeyV,
            b'W' => K::KeyW,
            b'X' => K::KeyX,
            b'Y' => K::KeyY,
            b'Z' => K::KeyZ,
            _ => return None,
        },
        _ => return None,
    };
    Some((cmd, shift, alt, key))
}

/// How an accelerator reads in a menu ("Ctrl+Shift+S").
pub fn accel_label(a: &str) -> String {
    a.replace("CmdOrCtrl", "Ctrl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_egui_app() {
        assert_eq!(code_name(KeyCode::KeyQ).as_deref(), Some("KeyQ"));
        assert_eq!(code_name(KeyCode::Numpad5).as_deref(), Some("Digit5"));
        assert_eq!(code_name(KeyCode::ArrowLeft).as_deref(), Some("Left"));
        assert_eq!(
            code_name(KeyCode::BracketLeft).as_deref(),
            Some("OpenBracket")
        );
        assert_eq!(code_name(KeyCode::NumpadEnter).as_deref(), Some("Enter"));
        assert_eq!(code_name(KeyCode::CapsLock), None);
    }

    #[test]
    fn modifiers_are_sorted() {
        let m = ModifiersState::SHIFT | ModifiersState::CONTROL | ModifiersState::ALT;
        assert_eq!(modifier_names(m), vec!["Alt", "Ctrl", "Shift"]);
    }

    #[test]
    fn accelerators_parse() {
        assert_eq!(
            parse_accel("CmdOrCtrl+Shift+S"),
            Some((true, true, false, KeyCode::KeyS))
        );
        assert_eq!(
            parse_accel("CmdOrCtrl+Period"),
            Some((true, false, false, KeyCode::Period))
        );
        assert!(parse_accel("Hyper+S").is_none());
    }
}
