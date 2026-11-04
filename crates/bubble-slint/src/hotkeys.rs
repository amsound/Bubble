//! Hot keys that never wait for the interface. On Windows a low-level keyboard hook,
//! on a thread of its own, sees each key before the window does. A key bound to a
//! trigger, pressed while the main window is in front and the keyboard is not wanted
//! for anything else, goes from there to a trigger thread and on to the controller,
//! as MIDI does; the window never sees it. Everything else (typing, Learn, menus,
//! shortcuts, Edit mode's arrows, Cart Setup) reaches the window as before.
//!
//! The interface says what the keyboard is wanted for in a [`KeyGate`], updated on
//! every refresh; the hook only reads it, and passes the key on if it cannot.

use std::sync::atomic::{AtomicBool, AtomicIsize};
use std::sync::{Arc, Mutex};

use slint::winit_030::winit::keyboard::KeyCode;

/// What the interface wants the keyboard for, as of its last refresh.
#[derive(Default)]
pub struct KeyGate {
    /// The main window (its HWND; 0 before it exists).
    pub window: AtomicIsize,
    /// Keys may trigger: a session is shown and nothing has the keyboard (no typing,
    /// Learn, quick edit, rename, dialog or open menu).
    pub live: AtomicBool,
    /// Edit mode: the arrows and Return move the cart cursor and open quick edit.
    pub edit_mode: AtomicBool,
    /// Key names bound to something in the session ("F1", "Escape"…).
    pub bound: Mutex<Vec<String>>,
    /// Menu shortcuts without Ctrl or Alt: (Shift, key).
    pub shortcuts: Mutex<Vec<(bool, KeyCode)>>,
}

impl KeyGate {
    /// Sets the bound keys, if they changed.
    pub fn set_bound(&self, mut names: Vec<String>) {
        names.sort();
        names.dedup();
        if let Ok(mut b) = self.bound.lock()
            && *b != names
        {
            *b = names;
        }
    }
}

/// Starts the hook and trigger threads (Windows; elsewhere keys go through the window).
pub fn start(
    gate: Arc<KeyGate>,
    ctrl: Arc<Mutex<bubble_control::Controller>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) {
    #[cfg(windows)]
    imp::start(gate, ctrl, repaint);
    #[cfg(not(windows))]
    let _ = (gate, ctrl, repaint);
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::HashSet;
    use std::sync::OnceLock;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::mpsc::{Sender, channel};

    use bubble_control::{Controller, InputEvent};
    use slint::winit_030::winit::keyboard::PhysicalKey;
    use slint::winit_030::winit::platform::scancode::PhysicalKeyExtScancode;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, HC_ACTION,
        KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, MSG, SetWindowsHookExW, TranslateMessage,
        WH_KEYBOARD_LL,
    };

    struct Hook {
        gate: Arc<KeyGate>,
        send: Mutex<Sender<InputEvent>>,
        /// Keys whose press the hook took: their release is taken too.
        down: Mutex<HashSet<KeyCode>>,
    }

    static HOOK: OnceLock<Hook> = OnceLock::new();

    pub fn start(
        gate: Arc<KeyGate>,
        ctrl: Arc<Mutex<Controller>>,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) {
        let (tx, rx) = channel::<InputEvent>();
        if HOOK
            .set(Hook {
                gate,
                send: Mutex::new(tx),
                down: Mutex::new(HashSet::new()),
            })
            .is_err()
        {
            return;
        }
        let triggers = std::thread::Builder::new()
            .name("bubble-hotkeys".into())
            .spawn(move || {
                // SAFETY: raises this thread's own priority.
                unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
                let probe = std::env::var_os("BUBBLE_PROBE").is_some();
                for ev in rx {
                    // As for MIDI: a panic elsewhere must not silence the keys for good.
                    let mut c = ctrl.lock().unwrap_or_else(|p| p.into_inner());
                    let used = c.handle_input(&ev);
                    drop(c);
                    if probe {
                        bubble_core::log::info(&format!(
                            "probe: hot key thread: {ev:?}, used {used}"
                        ));
                    }
                    repaint();
                }
            });
        if let Err(e) = triggers {
            bubble_core::log::info(&format!("hot keys go through the window: {e}"));
            return;
        }
        let hook = std::thread::Builder::new()
            .name("bubble-key-hook".into())
            .spawn(|| {
                // SAFETY: raises this thread's own priority; Windows drops a hook
                // that answers too slowly.
                unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
                // SAFETY: a hook procedure valid for the whole run, in this module.
                let h = unsafe {
                    SetWindowsHookExW(
                        WH_KEYBOARD_LL,
                        Some(proc),
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    )
                };
                if h.is_null() {
                    bubble_core::log::info("no keyboard hook: hot keys go through the window");
                    return;
                }
                bubble_core::log::info("hot keys: on their own thread");
                // The hook is called through this thread's messages.
                let mut msg: MSG = unsafe { std::mem::zeroed() };
                // SAFETY: this thread's own message loop.
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            });
        if let Err(e) = hook {
            bubble_core::log::info(&format!("hot keys go through the window: {e}"));
        }
    }

    fn held(vk: u16) -> bool {
        // SAFETY: reads the key's state.
        (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
    }

    /// Whether the hook takes this key: true sends it to the trigger thread and keeps
    /// it from the window.
    fn take(h: &Hook, code: KeyCode, up: bool) -> bool {
        let gate = &h.gate;
        let Ok(mut down) = h.down.try_lock() else {
            return false;
        };
        let send = |pressed: bool, repeat: bool, shift: bool| {
            let Some(name) = crate::keys::code_name(code) else {
                return;
            };
            let modifiers = if shift {
                vec!["Shift".to_string()]
            } else {
                Vec::new()
            };
            if let Ok(tx) = h.send.lock() {
                let _ = tx.send(InputEvent::Key {
                    code: name,
                    modifiers,
                    pressed,
                    repeat,
                });
            }
        };
        let front = gate.window.load(Relaxed) != 0
            // SAFETY: reads which window is in front.
            && unsafe { GetForegroundWindow() } as isize == gate.window.load(Relaxed);
        if up {
            // A press the hook took: its release goes the same way, even if Bubble is
            // no longer in front (a held cart must let go).
            if down.remove(&code) {
                send(false, false, false);
                return front;
            }
            return false;
        }
        // A menu has the keyboard while it is open (Esc closes it).
        if !front || !gate.live.load(Relaxed) || crate::window::MENU_OPEN.load(Relaxed) {
            return false;
        }
        if held(VK_CONTROL) || held(VK_MENU) || held(VK_LWIN) || held(VK_RWIN) {
            return false;
        }
        if gate.edit_mode.load(Relaxed)
            && matches!(
                code,
                KeyCode::ArrowLeft
                    | KeyCode::ArrowRight
                    | KeyCode::ArrowUp
                    | KeyCode::ArrowDown
                    | KeyCode::Enter
                    | KeyCode::NumpadEnter
            )
        {
            return false;
        }
        let shift = held(VK_SHIFT);
        let Some(name) = crate::keys::code_name(code) else {
            return false;
        };
        let bound = gate.bound.try_lock().is_ok_and(|b| b.contains(&name));
        let shortcut = gate
            .shortcuts
            .try_lock()
            .map_or(true, |s| s.contains(&(shift, code)));
        if !bound || shortcut {
            return false;
        }
        let repeat = !down.insert(code);
        send(true, repeat, shift);
        true
    }

    unsafe extern "system" fn proc(n: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if n == HC_ACTION as i32
            && let Some(h) = HOOK.get()
        {
            // SAFETY: for HC_ACTION, lparam points at the key's KBDLLHOOKSTRUCT.
            let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let scan = if k.flags & LLKHF_EXTENDED != 0 {
                0xE000 | k.scanCode
            } else {
                k.scanCode
            };
            if let PhysicalKey::Code(code) = PhysicalKey::from_scancode(scan)
                && take(h, code, k.flags & LLKHF_UP != 0)
            {
                return 1;
            }
        }
        // SAFETY: passes the key on to the next hook, as a hook must.
        unsafe { CallNextHookEx(std::ptr::null_mut(), n, wparam, lparam) }
    }
}
