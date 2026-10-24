//! The main window's behaviour on top of [`Core`]: input from the window becomes
//! [`Intent`]s, and every change shows through [`App::tick`], which also does the
//! core's per-frame work (audio watch, opening, jobs, autosave).
//!
//! Nothing here blocks: a trigger is sent to the controller as soon as the press
//! arrives, file dialogs and long jobs run on helper threads (the core's), and the
//! interface only redraws what changed.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bubble_control::{CartState as Cs, InputEvent, MouseButton};
use bubble_core::app::{AfterCreate, Core};
use bubble_core::dialogs::{AUDIO_EXTS, OPEN_EXTS, Purpose};
use bubble_core::menu::{Cmd, MenuState};
use bubble_core::prefs::Appearance;
use bubble_core::view::{first_empty_cell, free_cell_after, step_cell};
use bubble_core::{Intent, PendingAction, Snapshot};
use bubble_project::{Cart, FadeSpec, StopScope};
use slint::winit_030::winit::event::{ElementState, WindowEvent};
use slint::winit_030::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use uuid::Uuid;

use crate::alerts::{Alert, Alerts};
use crate::model::{self, Selection};
use crate::{
    BusyRow, MainWindow, PageTab, PopupRow, StatusItem, TextCommand, Theme, Tile, keys, menu,
    window,
};
use crate::{cartsetup, colour, savepanel};

/// How often the window refreshes while carts play (the clock and progress bars).
const PLAYING: Duration = Duration::from_millis(33);
/// While audio loads or a job runs.
const BUSY: Duration = Duration::from_millis(100);
/// While files are dragged over the window (the highlight follows the pointer).
const DRAGGING: Duration = Duration::from_millis(30);
/// Otherwise: autosave, cloud checks and the status line keep going.
const IDLE: Duration = Duration::from_secs(1);

thread_local! {
    /// Runs a tick on the UI thread when another thread wakes the window.
    static WAKE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// What an entry in the popup menu does.
#[derive(Clone)]
enum Popup {
    Separator,
    Assign(usize, u16, u16),
    PasteCart(usize, u16, u16),
    Control(usize, u16, u16, bubble_project::Control, Option<Uuid>),
    Rename(usize),
    CopyPage(usize),
    PastePage(usize),
    StopPage(usize),
    AddPage,
    RemovePage(usize),
    /// Shown but not available here yet.
    NotHere,
    /// A quick edit pop-up's choice ("output", "fade" or "group", and its index).
    Choice(&'static str, usize),
}

/// A context-menu entry: what it does, its text, and whether it is enabled.
type PopupItem = (Popup, String, bool);

/// The dialog on screen (one at a time, most urgent first).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dialog {
    Error,
    ConfirmClose,
    Recover,
    ConfirmDiscard,
    ImportChoice,
    NewSession,
    Relink,
    DeletePage,
    /// Windows: the new session's Audio files setting, asked after the Save dialog
    /// (the Mac asks it inside the Save panel).
    AudioFiles,
    /// No session to reopen (a first launch, or the last one is gone): New Session,
    /// Open or Quit. It comes back until there is a session.
    Start,
    /// Cart Setup's close button with edits not kept: Keep, Discard or Cancel.
    SetupClose,
}

pub struct App {
    core: Core,
    ui: slint::Weak<MainWindow>,
    edit_mode: bool,
    selected: Option<Uuid>,
    /// Keyboard cursor on an empty cell (arrow keys); a cart cell uses `selected`.
    cursor_empty: Option<(usize, u16, u16)>,
    /// The cart pressed with the mouse (released on pointer up).
    pressed: Option<Uuid>,
    /// The carts in the current page's cells, as shown.
    ids: Vec<Option<Uuid>>,
    page_shown: usize,
    tiles: Rc<VecModel<Tile>>,
    pages: Rc<VecModel<PageTab>>,
    status: Rc<VecModel<StatusItem>>,
    busy: Rc<VecModel<BusyRow>>,
    menu_sig: u64,
    menu_cmds: Vec<Cmd>,
    shortcuts: menu::Shortcuts,
    popup: Vec<Popup>,
    /// Quick edit: the cart, its settings as last shown, and the snapshot's last.
    quick_edit: Option<Uuid>,
    qe_cart: Option<Cart>,
    qe_seen: Option<Cart>,
    qe_soft: bool,
    qe_gen: i32,
    renaming: Option<usize>,
    dialog: Option<Dialog>,
    /// The native alert on screen, and which prompt it asks.
    alerts: Alerts,
    alert_for: Option<Dialog>,
    /// The system colour picker, for quick edit's or Cart Setup's other colour.
    picker: colour::Picker,
    picker_setup: bool,
    /// The Cart Setup window, and its close button asking about unkept edits.
    setup: Option<cartsetup::Setup>,
    setup_close_ask: bool,
    /// This app, for windows opened later to call back into.
    me: std::rc::Weak<RefCell<App>>,
    /// The system Save panel for a new session: open, its answer, and the Audio files
    /// question still to ask (Windows).
    save_open: bool,
    save_tx: std::sync::mpsc::Sender<Option<savepanel::Choice>>,
    save_rx: std::sync::mpsc::Receiver<Option<savepanel::Choice>>,
    ask_audio: bool,
    confirm_close: bool,
    allow_close: bool,
    relink: bool,
    /// Delete Page… asked for this page; the dialog confirms.
    delete_page: Option<usize>,
    place: bubble_core::view::PlaceTracker,
    mods: ModifiersState,
    /// Files dragged over the window, and files dropped (handled together).
    drag_hover: bool,
    drops: Vec<PathBuf>,
    last_title: String,
    exited: bool,
    first_tick: bool,
    /// The next refresh, chosen after each tick (input ticks too, so a cart that just
    /// started gets the playing rate at once, not after the idle interval).
    interval: Duration,
    timer: std::rc::Weak<slint::Timer>,
    /// The last input: the engine reports a cart as playing a moment after the press,
    /// so the window refreshes at the playing rate for a while after any input.
    last_input: Instant,
    /// When something last played. Play Next starts the next cart as the last one
    /// ends, and the engine reports it playing a moment later: in between nothing
    /// plays, so the playing rate is kept a little longer, or that tile would wait
    /// for the idle refresh.
    last_playing: Instant,
}

impl std::ops::Deref for App {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.core
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut Core {
        &mut self.core
    }
}

/// A problem message as an alert's bold headline and the detail under it: its first
/// line, or what comes before the first ": " when it is all one line.
fn headline(msg: &str) -> (String, String) {
    let msg = msg.trim();
    if let Some((head, rest)) = msg.split_once('\n') {
        return (head.trim().to_string(), rest.trim().to_string());
    }
    match msg.split_once(": ") {
        Some((head, rest)) if head.len() <= 80 => (format!("{head}."), rest.to_string()),
        _ => (msg.to_string(), String::new()),
    }
}

/// A label in this platform's style: Title Case on a Mac, sentence case on Windows.
fn name(mac: &str, other: &str) -> String {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
    .to_string()
}

/// Opens the window and runs until it closes.
pub fn run(renderer: &str, ui_font: &str) -> Result<(), slint::PlatformError> {
    let ui = MainWindow::new()?;
    ui.set_window_icon(window::icon());
    ui.global::<Theme>().set_mac(cfg!(target_os = "macos"));
    ui.global::<Theme>().set_ui_font(ui_font.into());

    // Other threads (jobs, dialogs, device scans, MIDI) wake the window this way.
    let pending = Arc::new(AtomicBool::new(false));
    let repaint: Arc<dyn Fn() + Send + Sync> = {
        let pending = pending.clone();
        Arc::new(move || {
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = slint::invoke_from_event_loop(|| {
                    WAKE.with(|w| {
                        if let Some(f) = w.borrow().as_ref() {
                            f();
                        }
                    })
                });
            }
        })
    };
    let mut core = Core::new(repaint);
    // First run on a touch screen: the Medium page bar, sized for fingers.
    if core.prefs.window.is_none() && core.prefs.bar_size == 0 && window::has_touch_screen() {
        core.prefs.bar_size = 1;
    }
    window::restore_place(&ui, core.prefs.window);
    ui.global::<Theme>()
        .set_appearance(appearance_index(core.prefs.appearance));
    ui.set_bar_height(bar_height(core.prefs.bar_size));

    let app = Rc::new(RefCell::new(App::new(core, &ui)));
    app.borrow_mut().me = Rc::downgrade(&app);
    // Audio, MIDI, recovery and the session to open (in the background).
    app.borrow_mut().core.start();
    log_info(&format!("window ready ({renderer})"));

    let timer = Rc::new(slint::Timer::default());
    app.borrow_mut().timer = Rc::downgrade(&timer);
    {
        let a = app.clone();
        let tick = move || {
            pending.store(false, Ordering::Release);
            with(&a, |app| app.tick());
        };
        let tick: Rc<dyn Fn()> = Rc::new(tick);
        let t2 = tick.clone();
        WAKE.with(|w| *w.borrow_mut() = Some(Box::new(move || t2())));
        timer.start(slint::TimerMode::Repeated, IDLE, move || tick());
    }
    wire(&ui, &app);
    hook_window_events(&ui, &app);

    ui.show()?;
    // The first tick runs at once (the session opening, the window's place).
    WAKE.with(|w| {
        if let Some(f) = w.borrow().as_ref() {
            f();
        }
    });
    let result = slint::run_event_loop();
    if let Ok(mut a) = app.try_borrow_mut() {
        a.on_exit();
    }
    WAKE.with(|w| *w.borrow_mut() = None);
    result
}

trait RestartWith {
    fn restart_with(&self, d: Duration);
}

impl RestartWith for slint::Timer {
    fn restart_with(&self, d: Duration) {
        if self.interval() != d {
            self.set_interval(d);
        }
    }
}

fn log_info(s: &str) {
    bubble_core::log::info(s);
}

fn appearance_index(a: Appearance) -> i32 {
    match a {
        Appearance::System => 0,
        Appearance::Light => 1,
        Appearance::Dark => 2,
    }
}

fn bar_height(size: u8) -> f32 {
    match size {
        0 => 44.0,
        1 => 64.0,
        _ => 92.0,
    }
}

/// Runs `f` with the app, then applies what it asked for and refreshes. If the app
/// is busy (a callback from inside a callback), `f` runs right after instead.
fn with<R: 'static>(app: &Rc<RefCell<App>>, f: impl FnOnce(&mut App) -> R + 'static) -> Option<R> {
    match app.try_borrow_mut() {
        Ok(mut a) => Some(f(&mut a)),
        Err(_) => {
            let a = app.clone();
            slint::Timer::single_shot(Duration::ZERO, move || {
                let _ = with(&a, f);
            });
            None
        }
    }
}

/// Input: act, then show the result at once.
fn act(app: &Rc<RefCell<App>>, f: impl FnOnce(&mut App, &mut Vec<Intent>) + 'static) {
    with(app, move |a| {
        a.last_input = Instant::now();
        let mut intents = Vec::new();
        f(a, &mut intents);
        a.apply_intents(intents);
        a.tick();
    });
}

macro_rules! on {
    ($ui:expr, $app:expr, $method:ident, |$a:ident, $i:ident $(, $arg:ident)*| $body:block) => {{
        let app = $app.clone();
        $ui.$method(move |$($arg),*| {
            act(&app, move |$a, $i| $body);
        });
    }};
}

/// Cart Setup's callbacks act on its `Setup` (taken out of the app while they run).
macro_rules! on_setup {
    ($win:expr, $me:expr, $method:ident, |$s:ident, $a:ident, $snap:ident, $i:ident $(, $arg:ident)*| $body:block) => {{
        let me = $me.clone();
        $win.$method(move |$($arg),*| {
            if let Some(app) = me.upgrade() {
                act(&app, move |$a, $i| {
                    let $snap = $a.core.snapshot();
                    if let Some(taken) = $a.setup.take() {
                        #[allow(unused_mut)]
                        let mut $s = taken;
                        $body
                        if $a.setup.is_none() {
                            $a.setup = Some($s);
                        }
                    }
                });
            }
        });
    }};
}

fn wire_setup(win: &crate::CartSetupWindow, me: &std::rc::Weak<RefCell<App>>) {
    for keep in [true, false] {
        let me = me.clone();
        let f = move || {
            if let Some(app) = me.upgrade() {
                act(&app, move |a, i| a.close_setup(keep, i));
            }
        };
        if keep {
            win.on_ok(f);
        } else {
            win.on_cancel(f);
        }
    }
    on_setup!(win, me, on_gain_changed, |s, a, snap, i, v| {
        s.gain(v, &snap, i);
    });
    on_setup!(win, me, on_pan_changed, |s, a, _snap, _i, v| {
        s.pan(v);
    });
    on_setup!(win, me, on_speed_changed, |s, a, snap, i, v| {
        s.speed(v, &snap, i);
    });
    on_setup!(win, me, on_reset, |s, a, snap, i, what| {
        s.reset(what.as_str(), &snap, i);
    });
    on_setup!(win, me, on_toggle, |s, a, snap, i, what| {
        s.toggle(what.as_str(), &snap, i);
    });
    on_setup!(win, me, on_label_edited, |s, a, _snap, _i, t| {
        s.draft.label = t.to_string();
    });
    on_setup!(win, me, on_notes_edited, |s, a, _snap, _i, t| {
        s.draft.notes = t.to_string();
    });
    on_setup!(win, me, on_colour, |s, a, _snap, _i, idx| {
        if let Some(c) = model::SWATCHES.get(idx as usize).copied() {
            s.set_colour(Some(c));
        }
    });
    on_setup!(win, me, on_pick_colour, |s, a, _snap, _i| {
        let [r, g, b] = s.current_rgb();
        a.picker_setup = true;
        a.picker.open((r, g, b), s.win.window());
    });
    on_setup!(win, me, on_choice_menu, |s, a, snap, _i, field, x, y| {
        let rows = s.choices(field.as_str(), &a.core, &snap);
        s.win.set_popup_rows(ModelRc::new(VecModel::from(rows)));
        s.win.set_popup_x(x);
        s.win.set_popup_y(y);
        let w = s.win.as_weak();
        window::on_main(move || {
            if let Some(win) = w.upgrade() {
                win.invoke_open_popup();
            }
        });
    });
    on_setup!(win, me, on_popup_chosen, |s, a, _snap, _i, id| {
        s.chosen(id);
    });
    on_setup!(win, me, on_nudge, |s, a, snap, i, which, d| {
        s.nudge(which.as_str(), d as f64, &snap, i);
    });
    on_setup!(win, me, on_action, |s, a, snap, i, what| {
        if !s.action(what.as_str(), &snap, i) {
            let id = s.id;
            match what.as_str() {
                "learn" => {
                    if snap.learning.is_some_and(|l| l.cart == id) {
                        i.push(Intent::CancelLearn);
                    } else {
                        i.push(Intent::LearnDefault(id));
                    }
                }
                "clear-key" => i.push(Intent::ClearBindings(id, Some(true))),
                "clear-midi" => i.push(Intent::ClearBindings(id, Some(false))),
                "replace" => {
                    if let Some((page, row, col)) = snap.project.cart_position(id) {
                        a.core.files.request(Purpose::Assign {
                            page,
                            row,
                            col,
                            many: false,
                        });
                    }
                }
                "locate" => {
                    if let Some(asset) = s.draft.asset {
                        a.core.files.request(Purpose::Relink(asset));
                    }
                }
                _ => {}
            }
        }
    });
    // Esc is Cancel (or cancels Learn).
    let me4 = me.clone();
    win.on_key(move |name, shift| {
        let Some(app) = me4.upgrade() else { return };
        act(&app, move |a, i| {
            let snap = a.core.snapshot();
            if name.as_str() == "escape" {
                if snap.learning.is_some() {
                    i.push(Intent::CancelLearn);
                } else {
                    a.close_setup(false, i);
                }
            } else if name.as_str() == "return" {
                a.close_setup(true, i);
            } else if let Some(s) = &mut a.setup {
                s.key(name.as_str(), shift, &snap, i);
            }
        });
    });
    on_setup!(win, me, on_wave_down, |s, a, snap, _i, x, y, _touch| {
        s.wave_down(x, y, &snap);
    });
    on_setup!(win, me, on_wave_move, |s, a, snap, i, x, _y| {
        s.wave_move(x, &snap, i);
    });
    on_setup!(win, me, on_wave_up, |s, a, _snap, _i, x, _y| {
        s.wave_up(x);
    });
    on_setup!(win, me, on_wave_double, |s, a, snap, i, x| {
        s.wave_double(x, &snap, i);
    });
    on_setup!(
        win,
        me,
        on_wave_scroll,
        |s, a, _snap, _i, dx, dy, zoom, shift| {
            s.wave_scroll(dx, dy, zoom, shift);
        }
    );
    on_setup!(win, me, on_wave_resized, |s, a, _snap, _i| {});

    // The close button: edits not kept are asked about (OK and Cancel never ask).
    let me2 = me.clone();
    win.window().on_close_requested(move || {
        let Some(app) = me2.upgrade() else {
            return slint::CloseRequestResponse::HideWindow;
        };
        let Ok(mut a) = app.try_borrow_mut() else {
            return slint::CloseRequestResponse::KeepWindowShown;
        };
        if a.setup.as_ref().is_some_and(|s| s.changed()) {
            a.setup_close_ask = true;
            a.tick();
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            let mut intents = Vec::new();
            a.close_setup(false, &mut intents);
            a.apply_intents(intents);
            a.tick();
            slint::CloseRequestResponse::HideWindow
        }
    });

    // Pinch: a trackpad (macOS) and two fingers on a touch screen. While Learn
    // waits, keys go to the controller as they do from the main window.
    let me3 = me.clone();
    win.window().on_winit_window_event(move |_w, ev| {
        let Some(app) = me3.upgrade() else {
            return EventResult::Propagate;
        };
        match ev {
            WindowEvent::KeyboardInput { event, .. } => {
                let learning = with(&app, |a| a.lock().learning().is_some()).unwrap_or(false);
                if !learning {
                    return EventResult::Propagate;
                }
                if let PhysicalKey::Code(code) = event.physical_key {
                    let (pressed, repeat) = (event.state == ElementState::Pressed, event.repeat);
                    act(&app, move |a, i| a.key(code, pressed, repeat, i));
                }
                EventResult::PreventDefault
            }
            WindowEvent::PinchGesture { delta, .. } => {
                let d = *delta;
                act(&app, move |a, _| {
                    if let Some(s) = &mut a.setup {
                        s.pinch(d);
                    }
                });
                EventResult::PreventDefault
            }
            WindowEvent::Touch(t) => {
                use slint::winit_030::winit::event::TouchPhase;
                let down = matches!(t.phase, TouchPhase::Started | TouchPhase::Moved);
                let (id, x, y) = (t.id, t.location.x, t.location.y);
                let pinching = with(&app, move |a| {
                    a.setup.as_mut().is_some_and(|s| s.touch(id, x, y, down))
                })
                .unwrap_or(false);
                if pinching {
                    act(&app, |_, _| {});
                    EventResult::PreventDefault
                } else {
                    EventResult::Propagate
                }
            }
            _ => EventResult::Propagate,
        }
    });
}

fn wire(ui: &MainWindow, app: &Rc<RefCell<App>>) {
    on!(ui, app, on_menu_activated, |a, i, id| {
        if let Some(cmd) = usize::try_from(id)
            .ok()
            .and_then(|id| a.menu_cmds.get(id).cloned())
        {
            a.command(cmd, i);
        }
        // A checkable item flips its own mark; rebuild so the marks follow the state.
        a.menu_sig = 0;
    });
    on!(
        ui,
        app,
        on_cell_down,
        |a, i, idx, right, touch, shift, alt, ctrl| {
            a.cell_down(idx, right, touch, shift, alt, ctrl, i);
        }
    );
    on!(ui, app, on_cell_up, |a, i, _idx, _touch| {
        if let Some(c) = a.pressed.take() {
            i.push(Intent::PointerUp(c));
        }
    });
    on!(ui, app, on_cell_tap, |a, i, idx| {
        a.cell_tap(idx, i);
    });
    on!(ui, app, on_cell_hold, |a, _i, idx| {
        a.cell_secondary(idx);
    });
    on!(ui, app, on_cell_double, |a, _i, idx| {
        if a.edit_mode
            && let Some((p, r, c)) = a.cell_pos(idx)
            && a.cart_at(idx).is_none()
        {
            a.files.request(Purpose::Assign {
                page: p,
                row: r,
                col: c,
                many: true,
            });
        }
    });
    on!(ui, app, on_cell_drop, |a, i, from, to, copy| {
        a.cell_drop(from, to, copy, i);
    });
    on!(ui, app, on_toggle_mode, |a, _i| {
        a.edit_mode = !a.edit_mode;
    });
    on!(ui, app, on_select_page, |a, i, p| {
        i.push(Intent::SetPage(p as usize));
        // The second click of a double-click (rename) selects the same page again.
        if a.renaming.is_some_and(|r| r != p as usize) {
            a.renaming = None;
            a.not_typing();
        }
    });
    on!(ui, app, on_start_rename, |a, _i, p| {
        a.start_rename(p as usize);
    });
    on!(ui, app, on_renamed, |a, i, p, text| {
        if a.renaming == Some(p as usize) {
            let name = text.trim().to_string();
            if !name.is_empty() {
                i.push(Intent::RenamePage(p as usize, name));
            }
            a.renaming = None;
            a.not_typing();
        }
    });
    on!(ui, app, on_page_menu, |a, _i, p, x, y| {
        a.page_menu(p as usize, x, y);
    });
    on!(ui, app, on_move_page, |_a, i, from, to| {
        if from != to {
            i.push(Intent::MovePage(from as usize, to as usize));
        }
    });
    on!(ui, app, on_add_page, |_a, i| {
        i.push(Intent::AddPage);
    });
    on!(ui, app, on_stop_page, |a, i| {
        let p = a.page_shown;
        i.push(Intent::StopPage(p));
    });
    on!(ui, app, on_stop_all, |_a, i| {
        i.push(Intent::StopAll(false));
    });
    on!(ui, app, on_status_clicked, |a, _i| {
        a.relink = true;
    });
    on!(ui, app, on_popup_chosen, |a, i, idx| {
        a.popup_chosen(idx as usize, i);
    });
    // Quick edit.
    on!(ui, app, on_qe_close, |a, _i| {
        a.close_quick_edit();
    });
    on!(ui, app, on_qe_label, |a, i, text| {
        a.qe_edit(i, move |c| c.label = text.to_string());
    });
    on!(ui, app, on_qe_colour, |a, i, idx| {
        if let Some(sw) = model::SWATCHES.get(idx as usize).copied() {
            a.qe_edit(i, move |c| {
                c.color = (sw != model::DEFAULT_CART_COLOR).then(|| model::hex(sw))
            });
            a.qe_refresh();
        }
    });
    on!(ui, app, on_qe_gain, |a, i, v| {
        // 0 dB is easy to hit.
        let db = if v.abs() < 0.25 {
            0.0
        } else {
            (v * 10.0).round() / 10.0
        };
        a.qe_edit(i, move |c| c.gain_db = db);
    });
    on!(ui, app, on_qe_choice_menu, |a, _i, field, x, y| {
        a.choice_menu(field.as_str(), x, y);
    });
    on!(ui, app, on_qe_flag, |a, i, field, on| {
        let field = field.to_string();
        a.qe_edit(i, move |c| match field.as_str() {
            "loop" => c.looping = on,
            "next" => c.play_next = on.then(|| "next_cell".to_string()),
            "stop-all" => c.on_play.stop = if on { StopScope::All } else { StopScope::None },
            _ => {}
        });
        a.qe_refresh();
    });
    on!(ui, app, on_qe_action, |a, i, action| {
        a.qe_action(action.as_str(), i);
    });
    // Closing with unsaved changes asks first.
    let a = app.clone();
    ui.window().on_close_requested(move || {
        let keep = with(&a, |app| {
            let dirty = app.lock().is_dirty();
            if dirty && !app.allow_close {
                app.confirm_close = true;
                app.tick();
                true
            } else {
                app.on_exit();
                false
            }
        });
        if keep.unwrap_or(true) {
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });
}

/// Window events Slint does not report: physical keys (hot keys), modifiers, focus,
/// the window's place, OS file drops, theme and scale changes.
fn hook_window_events(ui: &MainWindow, app: &Rc<RefCell<App>>) {
    let app = app.clone();
    // One full repaint after a minimise/restore (see `App::full_redraw`).
    let restored = Rc::new(Cell::new(false));
    ui.window().on_winit_window_event(move |_w, ev| {
        match ev {
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let (pressed, repeat) = (event.state == ElementState::Pressed, event.repeat);
                    act(&app, move |a, i| a.key(code, pressed, repeat, i));
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                let m = m.state();
                with(&app, move |a| a.mods = m);
            }
            WindowEvent::Focused(false) => {
                // Key releases cannot be seen while Bubble is in the background, so a
                // held hold-to-play cart would never stop: release them now.
                act(&app, |a, i| {
                    i.push(Intent::ReleaseHolds);
                    a.mods = ModifiersState::empty();
                });
            }
            WindowEvent::Focused(true) | WindowEvent::Occluded(false) => {
                with(&app, |a| a.full_redraw());
            }
            WindowEvent::Moved(_) => {
                with(&app, |a| a.track_place());
            }
            WindowEvent::Resized(size) => {
                // Minimised windows report a zero size on Windows.
                if size.width == 0 || size.height == 0 {
                    restored.set(true);
                } else if restored.replace(false) {
                    with(&app, |a| a.full_redraw());
                }
                with(&app, |a| a.track_place());
            }
            WindowEvent::HoveredFile(_) => {
                act(&app, |a, _| a.drag_hover = true);
            }
            WindowEvent::HoveredFileCancelled => {
                act(&app, |a, _| a.drag_hover = false);
            }
            WindowEvent::DroppedFile(p) => {
                // One event per file; take them all together once they have arrived.
                let p = p.clone();
                with(&app, move |a| a.drops.push(p));
                let a2 = app.clone();
                slint::Timer::single_shot(Duration::from_millis(10), move || {
                    act(&a2, |a, i| a.finish_drop(i));
                });
            }
            WindowEvent::ThemeChanged(t) => {
                let dark = *t == slint::winit_030::winit::window::Theme::Dark;
                with(&app, move |a| {
                    if let Some(ui) = a.ui.upgrade() {
                        ui.global::<Theme>().set_system_dark(dark);
                    }
                });
            }
            WindowEvent::Destroyed => {
                // The system is quitting Bubble (e.g. ⌘Q from the app menu): keep
                // unsaved work for recovery.
                with(&app, |a| a.on_exit());
            }
            _ => {}
        }
        EventResult::Propagate
    });
}

impl App {
    fn new(core: Core, ui: &MainWindow) -> Self {
        let tiles = Rc::new(VecModel::default());
        let pages = Rc::new(VecModel::default());
        let status = Rc::new(VecModel::default());
        let busy = Rc::new(VecModel::default());
        ui.set_busy(ModelRc::from(busy.clone()));
        ui.set_tiles(ModelRc::from(tiles.clone()));
        ui.set_pages(ModelRc::from(pages.clone()));
        ui.set_status(ModelRc::from(status.clone()));
        let (save_tx, save_rx) = std::sync::mpsc::channel();
        App {
            core,
            ui: ui.as_weak(),
            edit_mode: true,
            selected: None,
            cursor_empty: None,
            pressed: None,
            ids: Vec::new(),
            page_shown: 0,
            tiles,
            pages,
            status,
            busy,
            menu_sig: 0,
            menu_cmds: Vec::new(),
            shortcuts: menu::Shortcuts::new(&Default::default()),
            popup: Vec::new(),
            quick_edit: None,
            qe_cart: None,
            qe_seen: None,
            qe_soft: false,
            qe_gen: 0,
            renaming: None,
            dialog: None,
            alerts: Alerts::new(),
            alert_for: None,
            picker: colour::Picker::new(),
            picker_setup: false,
            setup: None,
            setup_close_ask: false,
            me: std::rc::Weak::new(),
            save_open: false,
            save_tx,
            save_rx,
            ask_audio: false,
            confirm_close: false,
            allow_close: false,
            relink: false,
            delete_page: None,
            place: Default::default(),
            mods: ModifiersState::empty(),
            drag_hover: false,
            drops: Vec::new(),
            last_title: String::new(),
            exited: false,
            first_tick: true,
            interval: IDLE,
            timer: std::rc::Weak::new(),
            last_input: Instant::now(),
            last_playing: Instant::now(),
        }
    }

    fn apply_intents(&mut self, intents: Vec<Intent>) {
        if !intents.is_empty() {
            self.core.apply(intents);
            self.take_selection();
        }
    }

    /// Applies a selection the core asked for (a cart just created, pasted or
    /// assigned; none after opening a session).
    fn take_selection(&mut self) {
        if let Some(s) = self.core.select.take() {
            self.selected = s;
            self.cursor_empty = None;
        }
    }

    /// One refresh: the core's per-frame work, then everything shown.
    fn tick(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if self.exited {
            return;
        }
        self.core.crash_test();
        if self.first_tick {
            self.first_tick = false;
            log_info("first frame shown");
            window::keep_on_screen(&ui);
            if let Some(dark) = window::system_dark(&ui) {
                ui.global::<Theme>().set_system_dark(dark);
            }
            window::set_title_bar_theme(&ui, self.prefs.appearance);
        }
        if cfg!(target_os = "macos")
            && let Some(dark) = window::system_dark(&ui)
            && ui.global::<Theme>().get_system_dark() != dark
        {
            ui.global::<Theme>().set_system_dark(dark);
        }
        let audio_wake = self.core.watch_audio();
        self.core.poll_devices();
        self.core.poll_open();
        self.take_selection();
        self.core.check_cloud();
        let mut snap = self.core.snapshot();
        self.core.note_loaded(&snap);

        let mut intents = Vec::new();
        if let Some(p) = self.core.open_at_start.take() {
            self.core.open_any(p, &snap, &mut intents);
        }
        // Later launches hand over here: come to the front, and open their file.
        let requests: Vec<_> = self.core.requests.try_iter().collect();
        for r in requests {
            window::bring_to_front(&ui);
            log_info(&format!("another launch asked to show Bubble ({r:?})"));
            if let Some(p) = r {
                self.core.open_any(p, &snap, &mut intents);
            }
        }
        // Before poll_job, so audio a finished job adds is in the project by now.
        self.core.wav_check_when_idle();
        self.core.poll_job(&mut intents);
        self.core.collect_when_idle();
        self.core.file_results(&snap, &mut intents);
        if let Some(i) = self.alerts.poll()
            && let Some(d) = self.alert_for.take()
        {
            self.alert_answer(d, i, &mut intents);
        }
        // The system colour picker: the quick edit cart, or Cart Setup's draft, follows it.
        if let Some(p) = self.picker.poll()
            && let Some((r, g, b)) = p.rgb
        {
            let rgb = [r, g, b];
            if self.picker_setup {
                if let Some(s) = &mut self.setup {
                    s.set_colour(Some(rgb));
                }
            } else if self.quick_edit.is_some() {
                self.qe_edit(&mut intents, |c| {
                    c.color = (rgb != model::DEFAULT_CART_COLOR).then(|| model::hex(rgb))
                });
                self.qe_refresh();
            }
        }
        // A handle dragged into Cart Setup's edge scrolls the waveform.
        if let Some(s) = &mut self.setup {
            s.auto_scroll(&snap, &mut intents);
        }
        if let Ok(choice) = self.save_rx.try_recv() {
            self.save_open = false;
            self.save_chosen(choice, &mut intents);
        }
        if !intents.is_empty() {
            self.apply_intents(intents);
            snap = self.core.snapshot();
        }
        // File panels hang on Cart Setup while it is open (it stays in front).
        match &self.setup {
            Some(s) => self.core.files.launch(&s.win.window().window_handle()),
            None => self.core.files.launch(&ui.window().window_handle()),
        }
        self.core.autosave();
        if self.place.due() {
            self.core.save_prefs();
        }

        self.show(&ui, &snap);

        let playing = snap
            .runtime
            .values()
            .any(|r| matches!(r.state, Cs::Playing | Cs::Stopping));
        if playing {
            self.last_playing = Instant::now();
        }
        self.interval = if self.drag_hover || self.setup.as_ref().is_some_and(|s| s.is_dragging()) {
            DRAGGING
        } else if self.last_playing.elapsed() < Duration::from_secs(2)
            || self.last_input.elapsed() < Duration::from_millis(600)
            // Cart Setup's meter falls, and its view follows scrolling, for a while after.
            || (self.setup.is_some() && self.last_input.elapsed() < Duration::from_secs(2))
        {
            PLAYING
        } else if snap.pending > 0
            || self.core.job.is_some()
            || self.core.opening.is_some()
            || self.picker.is_open()
        {
            BUSY
        } else if self.core.midi.active_recently(150) {
            // Clears the MIDI activity light.
            Duration::from_millis(200)
        } else if self.place.pending() {
            Duration::from_millis(500)
        } else {
            IDLE
        };
        if let Some(d) = audio_wake {
            self.interval = self.interval.min(d);
        }
        if let Some(t) = self.timer.upgrade() {
            t.restart_with(self.interval);
        }
    }

    /// Everything the window shows, from a snapshot. Models are only touched where
    /// something changed, so Slint redraws only that.
    fn show(&mut self, ui: &MainWindow, snap: &Snapshot) {
        // A selection that no longer exists (cart cleared, session changed) goes.
        if self
            .selected
            .is_some_and(|id| snap.project.cart(id).is_none())
        {
            self.selected = None;
        }
        if self.page_shown != snap.page {
            self.page_shown = snap.page;
            self.cursor_empty = None;
            if self.renaming.take().is_some() {
                self.not_typing();
            }
        }
        let title = self.core.window_title(snap);
        if title != self.last_title {
            ui.set_window_title(title.as_str().into());
            self.last_title = title;
        }

        // One busy popover for everything Bubble is doing (opening, copying,
        // importing, converting, loading audio); the tiles only get quiet cues.
        let rows = self.busy_rows(snap);
        let copying = self.core.job.as_ref().and_then(|j| j.cell);

        // Cart wall.
        let view = model::page_view(
            &self.core,
            snap,
            &Selection {
                cart: self.selected,
                empty: self.cursor_empty,
                copying,
            },
        );
        ui.set_rows(view.rows as i32);
        ui.set_cols(view.cols as i32);
        self.ids = view.ids;
        sync(&self.tiles, view.tiles);
        let style = model::label_style(&snap.project.settings.display);
        if ui.get_label_style() != style {
            ui.set_label_style(style);
        }
        ui.set_edit_mode(self.edit_mode);
        // No session yet (a first launch): the mark with New Session and Open.
        let drop = if self.drag_hover {
            window::pointer_in_window(ui).map_or(-1, |(x, y)| ui.invoke_cell_at(x, y))
        } else {
            -1
        };
        ui.set_drop_index(drop);

        // Bottom bar.
        sync(&self.pages, model::page_tabs(snap));
        ui.set_current_page(snap.page as i32);
        ui.set_renaming(self.renaming.map_or(-1, |i| i as i32));
        ui.set_bar_height(bar_height(self.prefs.bar_size));
        sync(&self.status, model::status(&self.core, snap, None));
        ui.set_status_clickable(!snap.missing.is_empty());
        sync(&self.busy, rows);

        self.show_menus(ui, snap);
        window::patch_app_menu();
        self.show_quick_edit(ui, snap);
        self.show_dialog(ui, snap);
        self.show_setup(ui, snap);
    }

    /// What the busy popover lists: the running job or the session being opened, then
    /// audio still loading (with a count).
    fn busy_rows(&mut self, snap: &Snapshot) -> Vec<BusyRow> {
        // Keeps the core's "n of total" count going.
        let _ = self.core.busy(snap);
        let mut rows = Vec::new();
        if let Some((p, _, _)) = &self.core.opening {
            rows.push(BusyRow {
                text: format!("Opening {}…", bubble_core::app::session::file_name(p)).into(),
                progress: -1.0,
                ..Default::default()
            });
        }
        if let Some(j) = &self.core.job {
            let detail = j.cell.and_then(|(p, r, c)| {
                let cols = snap.project.pages.get(p)?.cols;
                Some(format!("to cell {}", r * cols + c + 1))
            });
            rows.push(BusyRow {
                text: j.label.trim_end_matches('…').to_string().into(),
                detail: detail.unwrap_or_default().into(),
                copy: j.cell.is_some(),
                progress: -1.0,
            });
        }
        if snap.pending > 0 {
            let total = self.core.loading_total.max(snap.pending).max(1);
            let done = total - snap.pending.min(total);
            rows.push(BusyRow {
                text: "Loading audio…".into(),
                detail: format!("{done} of {total} files").into(),
                progress: done as f32 / total as f32,
                copy: false,
            });
        }
        rows
    }

    fn selection(&self, snap: &Snapshot) -> Option<Uuid> {
        self.selected.filter(|id| snap.project.cart(*id).is_some())
    }

    fn show_menus(&mut self, ui: &MainWindow, snap: &Snapshot) {
        let model = bubble_core::menu::build(
            &self.core,
            snap,
            &MenuState {
                edit_mode: self.edit_mode,
                selected: self.selection(snap),
                paste_target: self.paste_target(snap),
            },
        );
        let sig = model.signature();
        if sig == self.menu_sig {
            return;
        }
        self.menu_sig = sig;
        let built = menu::build(&model);
        self.menu_cmds = built.cmds;
        self.shortcuts = menu::Shortcuts::new(&model);
        // A new model rebuilds the menus, so check marks start from the state again.
        ui.set_menus(ModelRc::new(VecModel::from(built.tops)));
    }

    // ---- the cart wall ----

    fn cell_pos(&self, idx: i32) -> Option<(usize, u16, u16)> {
        let ui = self.ui.upgrade()?;
        let cols = ui.get_cols().max(1);
        (idx >= 0 && (idx as usize) < self.ids.len())
            .then(|| (self.page_shown, (idx / cols) as u16, (idx % cols) as u16))
    }

    fn cart_at(&self, idx: i32) -> Option<Uuid> {
        usize::try_from(idx)
            .ok()
            .and_then(|i| self.ids.get(i).copied().flatten())
    }

    #[allow(clippy::too_many_arguments)]
    fn cell_down(
        &mut self,
        idx: i32,
        right: bool,
        touch: bool,
        shift: bool,
        alt: bool,
        ctrl: bool,
        intents: &mut Vec<Intent>,
    ) {
        // macOS: Control-click is a right-click.
        let right = right || (cfg!(target_os = "macos") && ctrl && !touch);
        match self.cart_at(idx) {
            Some(cart) if right => {
                if self.edit_mode {
                    self.selected = Some(cart);
                    self.cursor_empty = None;
                    self.open_quick_edit(cart, idx);
                } else {
                    intents.push(Intent::PointerDown(cart, MouseButton::Secondary));
                }
            }
            // Shift-drag moves and Option-drag copies (handled on release).
            Some(_) if shift || alt => {}
            Some(cart) => {
                // Trigger on press, not on release.
                intents.push(Intent::PointerDown(cart, MouseButton::Primary));
                self.pressed = Some(cart);
                self.selected = Some(cart);
                self.cursor_empty = None;
            }
            None if right => self.cell_secondary(idx),
            None => {
                if let Some(pos) = self.cell_pos(idx)
                    && self.edit_mode
                {
                    self.selected = None;
                    self.cursor_empty = Some(pos);
                }
            }
        }
    }

    /// Touch in Edit mode: a tap selects and plays on release.
    fn cell_tap(&mut self, idx: i32, intents: &mut Vec<Intent>) {
        match self.cart_at(idx) {
            Some(cart) => {
                intents.push(Intent::PointerDown(cart, MouseButton::Primary));
                intents.push(Intent::PointerUp(cart));
                self.selected = Some(cart);
                self.cursor_empty = None;
            }
            None => {
                self.selected = None;
                self.cursor_empty = self.cell_pos(idx);
            }
        }
    }

    /// Right-click or press-and-hold in Edit mode: quick edit for a cart, the cell
    /// menu for an empty cell.
    fn cell_secondary(&mut self, idx: i32) {
        if !self.edit_mode {
            return;
        }
        match self.cart_at(idx) {
            Some(cart) => {
                self.selected = Some(cart);
                self.cursor_empty = None;
                self.open_quick_edit(cart, idx);
            }
            None => self.empty_cell_menu(idx),
        }
    }

    fn cell_drop(&mut self, from: i32, to: i32, copy: bool, intents: &mut Vec<Intent>) {
        if !self.edit_mode {
            return;
        }
        let (Some(a), Some(b)) = (self.cell_pos(from), self.cell_pos(to)) else {
            return;
        };
        if copy {
            if let Some(clip) = self.cart_at(from).and_then(|id| self.lock().copy_cart(id)) {
                intents.push(Intent::Paste(b.0, b.1, b.2, clip));
            }
        } else {
            intents.push(Intent::MoveCell(a, b));
        }
    }

    fn empty_cell_menu(&mut self, idx: i32) {
        let Some((p, r, c)) = self.cell_pos(idx) else {
            return;
        };
        let groups: Vec<(Uuid, String)> = {
            let ctrl = self.lock();
            ctrl.project()
                .groups
                .iter()
                .enumerate()
                .map(|(i, g)| (g.id, g.display_name(i)))
                .collect()
        };
        use bubble_project::Control as K;
        let head = vec![
            (
                Popup::Assign(p, r, c),
                name("Assign Audio Files…", "Assign audio files…"),
                true,
            ),
            (
                Popup::PasteCart(p, r, c),
                name("Paste Cart", "Paste cart"),
                self.clipboard.is_some(),
            ),
            (Popup::Separator, String::new(), false),
        ];
        let mut sub: Vec<PopupItem> = groups
            .into_iter()
            .map(|(id, label)| {
                let text = format!("Stop {label}");
                (Popup::Control(p, r, c, K::StopGroup, Some(id)), text, true)
            })
            .collect();
        if !sub.is_empty() {
            sub.push((Popup::Separator, String::new(), false));
        }
        sub.push((
            Popup::Control(p, r, c, K::StopPage, None),
            name("Stop Page", "Stop page"),
            true,
        ));
        sub.push((
            Popup::Control(p, r, c, K::StopAll, None),
            name("Stop All", "Stop all"),
            true,
        ));
        let Some(ui) = self.ui.upgrade() else { return };
        // At the pointer (or the press-and-hold point).
        let (x, y) = (ui.get_press_x(), ui.get_press_y());
        let sub_title = name("Add Control Button", "Add control button");
        self.show_popup(head, Some((sub_title, sub)), Vec::new(), x, y);
    }

    /// The page tab's menu: concise, one level, Delete last and set apart.
    fn page_menu(&mut self, page: usize, x: f32, y: f32) {
        let multi = self.lock().project().pages.len() > 1;
        let items = vec![
            (Popup::Rename(page), name("Rename…", "Rename…"), true),
            (
                Popup::NotHere,
                name(
                    "Page Settings… (not in this interface yet)",
                    "Page settings… (not in this interface yet)",
                ),
                false,
            ),
            (Popup::Separator, String::new(), false),
            (Popup::CopyPage(page), name("Copy Page", "Copy page"), true),
            (
                Popup::PastePage(page),
                name("Paste Page (Replaces It)", "Paste page (replaces it)"),
                self.page_clip.is_some(),
            ),
            (Popup::Separator, String::new(), false),
            (Popup::StopPage(page), name("Stop Page", "Stop page"), true),
            (Popup::AddPage, name("Add Page", "Add page"), true),
            (Popup::Separator, String::new(), false),
            (
                Popup::RemovePage(page),
                name("Delete Page…", "Delete page…"),
                multi,
            ),
        ];
        self.show_popup(items, None, Vec::new(), x, y);
    }

    /// Shows a native context menu at (x, y): `head`, then an optional submenu, then
    /// `tail`.
    fn show_popup(
        &mut self,
        head: Vec<PopupItem>,
        sub: Option<(String, Vec<PopupItem>)>,
        tail: Vec<PopupItem>,
        x: f32,
        y: f32,
    ) {
        let Some(ui) = self.ui.upgrade() else { return };
        self.popup.clear();
        let rows = |items: Vec<PopupItem>, all: &mut Vec<Popup>| {
            let rows: Vec<PopupRow> = items
                .into_iter()
                .map(|(p, text, enabled)| {
                    let separator = matches!(p, Popup::Separator);
                    all.push(p);
                    PopupRow {
                        id: all.len() as i32 - 1,
                        text: if separator {
                            menu::SEPARATOR.into()
                        } else {
                            text.into()
                        },
                        enabled: enabled && !separator,
                        checked: false,
                    }
                })
                .collect();
            ModelRc::new(VecModel::from(rows))
        };
        let mut all = Vec::new();
        ui.set_popup_rows(rows(head, &mut all));
        let (title, sub) = sub.unwrap_or_default();
        ui.set_popup_sub_title(title.into());
        ui.set_popup_sub(rows(sub, &mut all));
        ui.set_popup_tail(rows(tail, &mut all));
        self.popup = all;
        self.open_popup(&ui, x, y);
    }

    /// A quick edit pop-up button's choices as a native menu below it, the current
    /// one checked.
    fn choice_menu(&mut self, field: &str, x: f32, y: f32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let qe = ui.get_qe();
        let (field, list, current): (&'static str, _, _) = match field {
            "output" => ("output", qe.outputs, qe.output),
            "fade" => ("fade", qe.fades, qe.fade),
            _ => ("group", qe.groups, qe.group),
        };
        self.popup = (0..list.row_count())
            .map(|i| Popup::Choice(field, i))
            .collect();
        let rows: Vec<PopupRow> = list
            .iter()
            .enumerate()
            .map(|(i, text)| PopupRow {
                id: i as i32,
                text,
                enabled: true,
                checked: i as i32 == current,
            })
            .collect();
        ui.set_popup_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_popup_sub_title("".into());
        ui.set_popup_sub(ModelRc::default());
        ui.set_popup_tail(ModelRc::default());
        self.open_popup(&ui, x, y);
    }

    fn open_popup(&self, ui: &MainWindow, x: f32, y: f32) {
        ui.set_popup_x(x);
        ui.set_popup_y(y);
        // The system menu takes the mouse until it closes, so the button that opened
        // it is never released in the window, and the next click would go to the tab
        // or cell that opened it. End that press first, then open the menu, outside
        // this handler: its tracking loop runs Slint's timers, which must not happen
        // inside one (a timer opened menus before; with a cart playing that panicked).
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(x, y),
                button: slint::platform::PointerEventButton::Right,
            });
        let w = self.ui.clone();
        window::on_main(move || {
            if let Some(ui) = w.upgrade() {
                ui.invoke_open_popup();
            }
        });
    }

    fn popup_chosen(&mut self, idx: usize, intents: &mut Vec<Intent>) {
        let Some(p) = self.popup.get(idx).cloned() else {
            return;
        };
        match p {
            Popup::Separator | Popup::NotHere => {}
            Popup::Choice(field, i) => {
                self.qe_choice(field, i as i32, intents);
                self.qe_refresh();
            }
            Popup::Assign(page, row, col) => self.files.request(Purpose::Assign {
                page,
                row,
                col,
                many: true,
            }),
            Popup::PasteCart(p, r, c) => {
                if let Some(clip) = self.clipboard.clone() {
                    intents.push(Intent::Paste(p, r, c, clip));
                }
            }
            Popup::Control(p, r, c, k, g) => intents.push(Intent::CreateControl(p, r, c, k, g)),
            Popup::Rename(p) => self.start_rename(p),
            Popup::CopyPage(p) => {
                let clip = self.lock().copy_page(p);
                self.page_clip = clip;
            }
            Popup::PastePage(p) => {
                if let Some(clip) = self.page_clip.clone() {
                    intents.push(Intent::PastePage(p, clip));
                }
            }
            Popup::StopPage(p) => intents.push(Intent::StopPage(p)),
            Popup::AddPage => intents.push(Intent::AddPage),
            Popup::RemovePage(p) => self.delete_page = Some(p),
        }
    }

    fn start_rename(&mut self, page: usize) {
        let name = self
            .lock()
            .project()
            .pages
            .get(page)
            .map(|p| p.name.clone());
        if let (Some(name), Some(ui)) = (name, self.ui.upgrade()) {
            ui.set_rename_text(name.into());
            self.renaming = Some(page);
        }
    }

    /// Moves the keyboard cursor one cell (wrapping across rows).
    fn move_cursor(&mut self, snap: &Snapshot, dr: i32, dc: i32) {
        let Some(page) = snap.project.pages.get(snap.page) else {
            return;
        };
        let at = self
            .selected
            .and_then(|id| snap.project.cart_position(id))
            .filter(|(p, _, _)| *p == snap.page)
            .or(self.cursor_empty.filter(|(p, _, _)| *p == snap.page));
        let (r, c) = match at {
            Some((_, r, c)) => step_cell(page.rows, page.cols, (r, c), dr, dc),
            None => (0, 0),
        };
        match page.cell(r, c) {
            Some(cell) => {
                self.selected = Some(cell.cart.id);
                self.cursor_empty = None;
            }
            None => {
                self.selected = None;
                self.cursor_empty = Some((snap.page, r, c));
            }
        }
    }

    /// Where Paste goes: the selected cart's cell, the cursor, or the first empty cell.
    fn paste_target(&self, snap: &Snapshot) -> Option<(usize, u16, u16)> {
        if let Some(pos) = self.selected.and_then(|id| snap.project.cart_position(id)) {
            return Some(pos);
        }
        if let Some(pos) = self.cursor_empty.filter(|(p, _, _)| *p == snap.page) {
            return Some(pos);
        }
        first_empty_cell(&snap.project, snap.page)
    }

    // ---- keyboard ----

    fn key(&mut self, code: KeyCode, pressed: bool, repeat: bool, intents: &mut Vec<Intent>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let learning = self.lock().learning().is_some();
        let typing = ui.global::<TextCommand>().get_typing() && !learning;
        let command = keys::command(self.mods);
        // Menu shortcuts (the macOS menu handles its own), never while typing.
        if pressed
            && !typing
            && !cfg!(target_os = "macos")
            && let Some(cmd) = self.shortcuts.find(code, self.mods)
        {
            if !repeat {
                self.command(cmd, intents);
            }
            return;
        }
        if code == KeyCode::Escape && pressed {
            if self.quick_edit.is_some() {
                self.close_quick_edit();
                return;
            }
            if self.renaming.is_some() {
                self.renaming = None;
                self.not_typing();
                return;
            }
        }
        // Esc closes quick edit rather than stopping everything (release too).
        if code == KeyCode::Escape && self.quick_edit.is_some() {
            return;
        }
        // A text field keeps its key presses; Escape still reaches the hot keys (it
        // fades everything). Releases always go on, so a hold-to-play cart never sticks.
        if typing && pressed && code != KeyCode::Escape {
            return;
        }
        // Edit mode: arrows move the cart cursor, Return opens quick edit. They are
        // not passed to triggers there.
        if self.edit_mode
            && self.quick_edit.is_none()
            && self.dialog.is_none()
            && !command
            && !typing
        {
            let step = match code {
                KeyCode::ArrowLeft => Some((0, -1)),
                KeyCode::ArrowRight => Some((0, 1)),
                KeyCode::ArrowUp => Some((-1, 0)),
                KeyCode::ArrowDown => Some((1, 0)),
                _ => None,
            };
            if let Some((dr, dc)) = step {
                if pressed {
                    let snap = self.core.snapshot();
                    self.move_cursor(&snap, dr, dc);
                }
                return;
            }
            if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter) && !learning {
                if pressed
                    && let Some(id) = self.selected
                    && let Some(idx) = self.ids.iter().position(|c| *c == Some(id))
                {
                    self.open_quick_edit(id, idx as i32);
                }
                return;
            }
        }
        // App shortcuts with ⌘/Ctrl belong to the menus.
        if command && keys::menu_key(code) {
            return;
        }
        let Some(name) = keys::code_name(code) else {
            return;
        };
        intents.push(Intent::Input(InputEvent::Key {
            code: name,
            modifiers: keys::modifier_names(self.mods),
            pressed,
            repeat,
        }));
    }

    // ---- menu commands ----

    fn command(&mut self, cmd: Cmd, intents: &mut Vec<Intent>) {
        let snap = self.core.snapshot();
        if bubble_core::menu::apply_common(&mut self.core, &cmd, &snap, intents) {
            return;
        }
        // While typing, the edit commands act on the text field.
        let typing = self
            .ui
            .upgrade()
            .is_some_and(|ui| ui.global::<TextCommand>().get_typing());
        if typing {
            let text = match cmd {
                Cmd::Cut => Some("cut"),
                Cmd::Copy => Some("copy"),
                Cmd::Paste => Some("paste"),
                Cmd::Undo => Some("undo"),
                Cmd::Redo => Some("redo"),
                Cmd::PasteAttributes(_) | Cmd::DuplicateCart | Cmd::ClearCart => Some(""),
                _ => None,
            };
            if let Some(t) = text {
                if let (Some(ui), false) = (self.ui.upgrade(), t.is_empty()) {
                    ui.invoke_text_command(t.into());
                }
                return;
            }
        }
        let sel = self.selection(&snap);
        match cmd {
            Cmd::New => self.request_from_ui(PendingAction::New, intents),
            Cmd::Open => self.files.request(Purpose::Open),
            Cmd::OpenRecent(i) => {
                if let Some(p) = self.prefs.recent_projects.get(i).cloned() {
                    self.request_from_ui(PendingAction::Open(p), intents);
                }
            }
            Cmd::Save => self.core.save_or_save_as(&snap, intents),
            Cmd::SaveAs => self
                .core
                .begin_new_session(format!("{} copy", snap.project.name), AfterCreate::SaveAs),
            Cmd::Relink => self.relink = true,
            Cmd::ReloadAudio => intents.push(Intent::Refresh),
            Cmd::Quit => {
                if let Some(ui) = self.ui.upgrade() {
                    let w = ui.as_weak();
                    // After this callback: closing re-enters the app.
                    slint::Timer::single_shot(Duration::ZERO, move || {
                        if let Some(ui) = w.upgrade() {
                            ui.window()
                                .dispatch_event(slint::platform::WindowEvent::CloseRequested);
                        }
                    });
                }
            }
            Cmd::Undo => intents.push(Intent::Undo),
            Cmd::Redo => intents.push(Intent::Redo),
            Cmd::Copy | Cmd::Cut => {
                if let Some(id) = sel {
                    let clip = self.lock().copy_cart(id);
                    self.clipboard = clip;
                    if cmd == Cmd::Cut
                        && let Some((p, r, c)) = snap.project.cart_position(id)
                    {
                        intents.push(Intent::ClearCell(p, r, c));
                        self.selected = None;
                    }
                }
            }
            Cmd::Paste => {
                if let (Some(clip), Some((p, r, c))) =
                    (self.clipboard.clone(), self.paste_target(&snap))
                {
                    intents.push(Intent::Paste(p, r, c, clip));
                }
            }
            Cmd::PasteAttributes(what) => {
                if let (Some(clip), Some(sel)) = (self.clipboard.clone(), sel) {
                    intents.push(Intent::PasteAttrs(clip.cart, vec![sel], what.set()));
                }
            }
            Cmd::ClearCart => {
                if let Some((p, r, c)) = sel.and_then(|id| snap.project.cart_position(id)) {
                    intents.push(Intent::ClearCell(p, r, c));
                    self.selected = None;
                }
            }
            Cmd::DuplicateCart => {
                if let Some(id) = sel {
                    let free = free_cell_after(&snap.project, id);
                    let clip = self.lock().copy_cart(id);
                    match (free, clip) {
                        (Some((p, r, c)), Some(clip)) => intents.push(Intent::Paste(p, r, c, clip)),
                        (None, _) => self.prompts.error = Some("This page is full.".into()),
                        _ => {}
                    }
                }
            }
            Cmd::ToggleEditMode => self.edit_mode = !self.edit_mode,
            Cmd::AddPage => intents.push(Intent::AddPage),
            Cmd::RemovePage => intents.push(Intent::RemovePage(snap.page)),
            Cmd::PlaySelected => {
                if let Some(id) = sel {
                    intents.push(Intent::Trigger(id));
                }
            }
            Cmd::StopAll => intents.push(Intent::StopAll(true)),
            Cmd::StopPage => intents.push(Intent::StopPage(snap.page)),
            Cmd::StopAllNow => intents.push(Intent::StopAll(false)),
            Cmd::ShowLogs => {
                if let Some(d) = bubble_core::log::dir() {
                    bubble_core::dialogs::reveal(&d.join("bubble.log"));
                }
            }
            Cmd::RevealSession => {
                if let Some(p) = &snap.path {
                    bubble_core::dialogs::reveal(p);
                }
            }
            Cmd::CopyPage => {
                let clip = self.lock().copy_page(snap.page);
                self.page_clip = clip;
            }
            Cmd::PastePage => {
                if let Some(clip) = self.page_clip.clone() {
                    intents.push(Intent::PastePage(snap.page, clip));
                }
            }
            Cmd::Appearance(a) => {
                self.prefs.appearance = a;
                self.core.save_prefs();
                if let Some(ui) = self.ui.upgrade() {
                    ui.global::<Theme>().set_appearance(appearance_index(a));
                    window::set_title_bar_theme(&ui, a);
                    if let Some(dark) = window::system_dark(&ui).filter(|_| a == Appearance::System)
                    {
                        ui.global::<Theme>().set_system_dark(dark);
                    }
                }
            }
            Cmd::AudioSetup => {
                if let Some(id) = sel {
                    self.open_setup(id);
                }
            }
            // Windows not in this interface yet (their menu items are disabled).
            Cmd::SpeedBar
            | Cmd::Outputs
            | Cmd::ReassignOutputs
            | Cmd::ProjectSettings
            | Cmd::PageSettings
            | Cmd::ShowLog
            | Cmd::ShowDiagnostics => {}
            // Handled by `apply_common`.
            _ => {}
        }
    }

    /// New/Open: asks about unsaved changes first.
    fn request_from_ui(&mut self, a: PendingAction, intents: &mut Vec<Intent>) {
        let snap = self.core.snapshot();
        self.core.request(a, &snap, intents);
    }

    // ---- quick edit ----

    fn open_quick_edit(&mut self, cart: Uuid, idx: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        self.quick_edit = Some(cart);
        self.qe_cart = None;
        self.qe_seen = None;
        self.qe_soft = false;
        // Beside the cart (right of it, or left when there is no room).
        let width = ui
            .window()
            .size()
            .to_logical(ui.window().scale_factor())
            .width;
        let (x, w) = (ui.invoke_cell_x(idx), ui.invoke_cell_w());
        let panel = if cfg!(target_os = "macos") {
            392.0
        } else {
            404.0
        };
        let right = x + w + 10.0;
        let fits_right = right + panel <= width - 8.0;
        ui.set_qe_x(if fits_right { right } else { x - panel - 10.0 });
        ui.set_qe_arrow_left(fits_right);
        let y = ui.invoke_cell_y(idx);
        ui.set_qe_y(y);
        ui.set_qe_point_y(y + ui.invoke_cell_h() / 2.0);
    }

    // ---- Cart Setup ----

    /// Opens Cart Setup for a cart (a control button has none). Another cart's setup
    /// with edits not kept stays as it is and comes to the front.
    fn open_setup(&mut self, id: Uuid) {
        let Some(cart) = self
            .lock()
            .project()
            .cart(id)
            .cloned()
            .filter(|c| c.control.is_none())
        else {
            return;
        };
        if let Some(s) = &self.setup {
            if s.id == id || s.changed() {
                let _ = s.win.show();
                return;
            }
            let _ = s.win.hide();
        }
        let (Ok(win), Some(ui)) = (crate::CartSetupWindow::new(), self.ui.upgrade()) else {
            return;
        };
        let theme = ui.global::<Theme>();
        let t = win.global::<Theme>();
        t.set_mac(theme.get_mac());
        t.set_ui_font(theme.get_ui_font());
        t.set_appearance(theme.get_appearance());
        t.set_system_dark(theme.get_system_dark());
        win.set_window_icon(window::icon());
        wire_setup(&win, &self.me);
        let _ = win.show();
        window::keep_above(win.window(), ui.window());
        self.setup = Some(cartsetup::Setup::new(win, cart));
    }

    /// Closes Cart Setup: `keep` replaces the cart with the draft (one undo step),
    /// otherwise the draft is dropped and an audition stops.
    fn close_setup(&mut self, keep: bool, intents: &mut Vec<Intent>) {
        let Some(s) = self.setup.take() else { return };
        let snap = self.core.snapshot();
        let playing = snap
            .runtime
            .get(&s.id)
            .is_some_and(|r| matches!(r.state, Cs::Playing | Cs::Stopping));
        if keep && s.changed() {
            intents.push(Intent::ReplaceCart(s.draft.clone()));
        } else if !keep && playing {
            intents.push(Intent::KillCart(s.id));
        }
        if self.picker_setup {
            self.picker.close();
            self.picker_setup = false;
        }
        self.setup_close_ask = false;
        let _ = s.win.hide();
    }

    fn show_setup(&mut self, ui: &MainWindow, snap: &Snapshot) {
        let Some(mut s) = self.setup.take() else {
            return;
        };
        let (theme, t) = (ui.global::<Theme>(), s.win.global::<Theme>());
        if t.get_appearance() != theme.get_appearance() {
            t.set_appearance(theme.get_appearance());
        }
        if t.get_system_dark() != theme.get_system_dark() {
            t.set_system_dark(theme.get_system_dark());
        }
        s.show(&mut self.core, snap);
        self.setup = Some(s);
    }

    fn close_quick_edit(&mut self) {
        self.picker.close();
        self.quick_edit = None;
        self.qe_cart = None;
        self.qe_seen = None;
        self.qe_soft = false;
        self.not_typing();
    }

    /// A text field that had the keyboard is gone (its panel closed), and it reports no
    /// focus loss when it is removed: hot keys and arrows work again.
    fn not_typing(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<TextCommand>().set_typing(false);
        }
    }

    /// Shows the panel again from the cart's current settings (after a change made
    /// outside the panel's own fields, e.g. a reset or undo).
    /// Show the panel's own edit (a colour, a toggle) once it lands, without rebuilding
    /// the panel, so it does not fade in again.
    fn qe_refresh(&mut self) {
        self.qe_soft = true;
    }

    fn show_quick_edit(&mut self, ui: &MainWindow, snap: &Snapshot) {
        let cart = self
            .quick_edit
            .filter(|_| self.edit_mode)
            .and_then(|id| snap.project.cart(id).cloned());
        let Some(cart) = cart else {
            if self.quick_edit.take().is_some() || ui.get_qe_gen().row_count() > 0 {
                self.close_quick_edit();
                ui.set_qe_gen(ModelRc::default());
            }
            return;
        };
        if self.qe_soft && self.qe_cart.as_ref() == Some(&cart) {
            self.qe_soft = false;
            self.qe_seen = Some(cart.clone());
            ui.set_qe(model::quick_edit(&self.core, snap, &cart));
            return;
        }
        // Rebuild the panel only when the cart changed from outside it, so typing in
        // it is never interrupted.
        if self.qe_seen.as_ref() == Some(&cart) {
            return;
        }
        self.qe_seen = Some(cart.clone());
        if self.qe_cart.as_ref() == Some(&cart) {
            return;
        }
        self.qe_cart = Some(cart.clone());
        ui.set_qe(model::quick_edit(&self.core, snap, &cart));
        self.qe_gen += 1;
        ui.set_qe_gen(ModelRc::new(VecModel::from(vec![self.qe_gen])));
    }

    fn qe_edit(&mut self, intents: &mut Vec<Intent>, f: impl FnOnce(&mut Cart)) {
        let Some(cart) = self.qe_cart.clone() else {
            return;
        };
        let mut c = cart.clone();
        f(&mut c);
        if c != cart {
            self.qe_cart = Some(c.clone());
            intents.push(Intent::ReplaceCart(c));
        }
    }

    fn qe_choice(&mut self, field: &str, idx: i32, intents: &mut Vec<Intent>) {
        let Some(cart) = self.qe_cart.clone() else {
            return;
        };
        let idx = idx.max(0) as usize;
        match field {
            "output" => self.qe_edit(intents, |c| c.output = bubble_project::output_letter(idx)),
            "fade" => {
                let default = self.lock().project().settings.default_fade_out_ms;
                let (_, values, _) = model::fade_choices(&cart, default);
                if let Some(v) = values.get(idx).copied() {
                    let curve = cart.fade_out.map(|f| f.curve).unwrap_or_default();
                    self.qe_edit(intents, |c| {
                        c.fade_out = v.map(|ms| FadeSpec {
                            curve,
                            ..FadeSpec::ms(ms)
                        })
                    });
                }
            }
            "group" => {
                let group = (idx > 0)
                    .then(|| self.lock().project().groups.get(idx - 1).map(|g| g.id))
                    .flatten();
                self.qe_edit(intents, |c| c.group = group);
            }
            _ => {}
        }
    }

    fn qe_action(&mut self, action: &str, intents: &mut Vec<Intent>) {
        let Some(id) = self.quick_edit else { return };
        let at = self.lock().project().cart_position(id);
        match action {
            "done" => self.close_quick_edit(),
            "setup" => {
                self.close_quick_edit();
                self.open_setup(id);
            }
            "learn" => {
                intents.push(Intent::LearnDefault(id));
                self.close_quick_edit();
            }
            "replace" => {
                if let Some((page, row, col)) = at {
                    self.files.request(Purpose::Assign {
                        page,
                        row,
                        col,
                        many: false,
                    });
                }
                self.close_quick_edit();
            }
            "copy" => {
                let clip = self.lock().copy_cart(id);
                self.clipboard = clip;
                self.close_quick_edit();
            }
            "paste" => {
                if let (Some(clip), Some((p, r, c))) = (self.clipboard.clone(), at) {
                    intents.push(Intent::Paste(p, r, c, clip));
                }
                self.close_quick_edit();
            }
            "delete" => {
                if let Some((p, r, c)) = at {
                    intents.push(Intent::ClearCell(p, r, c));
                    self.selected = None;
                }
                self.close_quick_edit();
            }
            "gain-reset" => {
                self.qe_edit(intents, |c| c.gain_db = 0.0);
                self.qe_refresh();
            }
            "pick-colour" => {
                if let (Some(ui), Some(cart)) = (self.ui.upgrade(), self.qe_cart.as_ref()) {
                    let [r, g, b] = model::parse_rgb(cart.color.as_deref())
                        .unwrap_or(model::DEFAULT_CART_COLOR);
                    self.picker.open((r, g, b), ui.window());
                }
            }
            _ => {}
        }
    }

    // ---- dialogs ----

    fn current_dialog(&self) -> Option<Dialog> {
        let p = &self.core.prompts;
        if p.error.is_some() {
            Some(Dialog::Error)
        } else if self.setup_close_ask {
            Some(Dialog::SetupClose)
        } else if self.confirm_close {
            Some(Dialog::ConfirmClose)
        } else if !p.recover.is_empty() {
            Some(Dialog::Recover)
        } else if p.confirm_discard.is_some() {
            Some(Dialog::ConfirmDiscard)
        } else if p.import_choice.is_some() {
            Some(Dialog::ImportChoice)
        } else if self.ask_audio && p.new_session.is_some() {
            Some(Dialog::AudioFiles)
        } else if p.new_session.is_some() {
            Some(Dialog::NewSession)
        } else if self.delete_page.is_some() {
            Some(Dialog::DeletePage)
        } else if self.relink {
            Some(Dialog::Relink)
        } else if self.needs_session() {
            Some(Dialog::Start)
        } else {
            None
        }
    }

    /// Nothing open, being opened or imported, and nothing on the grid.
    fn needs_session(&self) -> bool {
        if self.first_tick
            || self.core.opening.is_some()
            || self.core.open_at_start.is_some()
            || self.core.job.is_some()
        {
            return false;
        }
        let c = self.lock();
        c.path().is_none() && c.project().carts().next().is_none()
    }

    fn show_dialog(&mut self, ui: &MainWindow, snap: &Snapshot) {
        // A session set to Convert to WAV converts without asking (chosen with it).
        if let Some(files) = self.core.prompts.convert_offer.take() {
            self.core.start_conversion(files);
        }
        let d = self.current_dialog();
        self.dialog = d;
        let busy = self.alerts.is_open() || self.save_open || self.core.files.is_open();
        // New Session, Save As, Import: the system Save panel.
        if d == Some(Dialog::NewSession) {
            if !busy && let Some(ns) = &self.core.prompts.new_session {
                let (title, button) = match ns.then {
                    AfterCreate::Import(_) => ("Import into a New Session", "Import"),
                    AfterCreate::SaveAs => ("Save Session As", "Save"),
                    _ => ("New Session", "Create"),
                };
                let req = savepanel::Request {
                    title: title.into(),
                    button: button.into(),
                    name: ns.name.clone(),
                    location: ns.location.clone(),
                    ask_audio: !matches!(ns.then, AfterCreate::SaveAs),
                };
                self.save_open = true;
                let tx = self.save_tx.clone();
                let wake = self.core.repaint.clone();
                savepanel::show(
                    req,
                    ui.window(),
                    Box::new(move |c| {
                        let _ = tx.send(c);
                        wake();
                    }),
                );
            }
            return;
        }
        // The system's own alert; one at a time, never over a file dialog.
        if let Some(d) = d
            && !busy
        {
            let alert = self.native_alert(d, snap);
            self.alert_for = Some(d);
            self.alerts
                .show(alert, ui.window(), self.core.repaint.clone());
        }
    }

    /// The prompt as a native alert. Buttons keep `dialog_button`'s order: the system
    /// puts Cancel last itself (NSAlert, TaskDialog).
    fn native_alert(&self, d: Dialog, snap: &Snapshot) -> Alert {
        let p = &self.core.prompts;
        let session = || {
            snap.path
                .as_deref()
                .and_then(|p| p.file_stem())
                .map_or("Untitled".to_string(), |n| n.to_string_lossy().to_string())
        };
        let buttons = |v: &[String]| v.to_vec();
        match d {
            Dialog::Error => {
                let (title, info) = headline(p.error.as_deref().unwrap_or_default());
                Alert {
                    title,
                    info,
                    buttons: buttons(&["OK".into(), name("Copy Details", "Copy details")]),
                    error: true,
                    ..Default::default()
                }
            }
            Dialog::ConfirmClose | Dialog::ConfirmDiscard => Alert {
                title: if cfg!(target_os = "macos") {
                    format!(
                        "Do you want to save the changes you made to \u{201c}{}\u{201d}?",
                        session()
                    )
                } else {
                    format!("Do you want to save changes to {}?", session())
                },
                info: name(
                    "Your changes will be lost if you don\u{2019}t save them.",
                    "",
                ),
                buttons: buttons(&[
                    "Save".into(),
                    "Cancel".into(),
                    name("Don\u{2019}t Save", "Don\u{2019}t save"),
                ]),
                ..Default::default()
            },
            Dialog::Recover => {
                let (file, project) = p.recover.first().cloned().unwrap_or_default();
                let n = project
                    .as_deref()
                    .unwrap_or(file.as_path())
                    .file_stem()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                Alert {
                    title: format!("Restore unsaved changes to \u{201c}{n}\u{201d}?"),
                    info: "Bubble closed before they were saved. Restoring doesn\u{2019}t change \
                           the session file until you save."
                        .into(),
                    buttons: buttons(&[
                        "Restore".into(),
                        name("Discard Changes", "Discard changes"),
                    ]),
                    destructive: Some(1),
                    ..Default::default()
                }
            }
            Dialog::ImportChoice => {
                let n = p
                    .import_choice
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                Alert {
                    title: format!("Import \u{201c}{n}\u{201d}?"),
                    info: "Add its carts to the open session, or start a new session from it."
                        .into(),
                    buttons: buttons(&[
                        name("Add to This Session", "Add to this session"),
                        "Cancel".into(),
                        name("New Session\u{2026}", "Start a new session\u{2026}"),
                    ]),
                    links: true,
                    ..Default::default()
                }
            }
            Dialog::AudioFiles => {
                let n = p
                    .new_session
                    .as_ref()
                    .map(|ns| ns.name.clone())
                    .unwrap_or_default();
                Alert {
                    title: format!("How should \u{201c}{n}\u{201d} keep its audio files?"),
                    info: "Convert to WAV converts each file once, when it is added, so the \
                           session opens faster on slower computers. Playback is the same \
                           either way."
                        .into(),
                    buttons: vec![
                        name("Keep as Imported", "Keep as imported"),
                        name("Convert to WAV", "Convert to WAV"),
                    ],
                    links: true,
                    ..Default::default()
                }
            }
            Dialog::Start => Alert {
                title: "Start with a session".into(),
                info: "Bubble keeps its carts and audio in a session. Create a new one, or \
                       open one you already have."
                    .into(),
                buttons: vec![
                    name("New Session\u{2026}", "New session\u{2026}"),
                    "Open\u{2026}".into(),
                    name("Quit", "Exit"),
                ],
                links: true,
                ..Default::default()
            },
            Dialog::SetupClose => {
                let n = self
                    .setup
                    .as_ref()
                    .map(|s| bubble_control::display_label(&s.draft, &snap.project))
                    .unwrap_or_default();
                Alert {
                    title: format!("Keep the changes to \u{201c}{n}\u{201d}?"),
                    info: "You changed this cart\u{2019}s setup and have not pressed OK.".into(),
                    buttons: vec!["Keep".into(), "Cancel".into(), "Discard".into()],
                    destructive: Some(2),
                    ..Default::default()
                }
            }
            Dialog::DeletePage => {
                let page = self.delete_page.unwrap_or(0);
                let n = snap
                    .project
                    .pages
                    .get(page)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                Alert {
                    title: format!("Delete the page \u{201c}{n}\u{201d}?"),
                    info: "Its carts are deleted with it. You can undo this.".into(),
                    buttons: buttons(&["Delete".into(), "Cancel".into()]),
                    destructive: Some(0),
                    ..Default::default()
                }
            }
            Dialog::Relink => {
                let names: Vec<String> = snap
                    .missing
                    .iter()
                    .filter_map(|a| snap.project.asset(*a))
                    .map(|a| {
                        let f = std::path::Path::new(&a.path)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| a.path.clone());
                        format!("\u{201c}{f}\u{201d}")
                    })
                    .collect();
                let n = names.len();
                let list = match n {
                    0 => String::new(),
                    1 => names[0].clone(),
                    2 => format!("{} and {}", names[0], names[1]),
                    _ => format!("{}, {} and {} more", names[0], names[1], n - 2),
                };
                Alert {
                    title: if n == 1 {
                        "1 audio file is missing.".into()
                    } else {
                        format!("{n} audio files are missing.")
                    },
                    info: if n == 1 {
                        format!("{list} was moved, renamed or deleted.")
                    } else {
                        format!(
                            "{list}. Locate one and Bubble also finds others that moved with it."
                        )
                    },
                    buttons: buttons(&[name("Locate\u{2026}", "Locate\u{2026}"), "Later".into()]),
                    ..Default::default()
                }
            }
            _ => Alert::default(),
        }
    }

    /// The Save panel closed: create the session there, or ask Audio files first.
    fn save_chosen(&mut self, choice: Option<savepanel::Choice>, intents: &mut Vec<Intent>) {
        let Some(c) = choice else {
            self.core.prompts.new_session = None;
            return;
        };
        let Some(dir) = c.path.parent().map(|p| p.to_path_buf()) else {
            return;
        };
        // The name as typed (a Mac panel may add no extension; drop one if it did).
        let typed = c
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = typed.strip_suffix(".bubble").unwrap_or(&typed).to_string();
        self.core.set_session_location(dir);
        let Some(ns) = &mut self.core.prompts.new_session else {
            return;
        };
        ns.name = name;
        let ask = !matches!(ns.then, AfterCreate::SaveAs);
        match c.convert {
            Some(wav) => {
                ns.convert_to_wav = wav;
                self.create_new_session(intents);
            }
            None if ask => self.ask_audio = true,
            None => self.create_new_session(intents),
        }
    }

    /// Creates the session; if that fails (a session already there), says why.
    fn create_new_session(&mut self, intents: &mut Vec<Intent>) {
        self.core.create_session(intents);
        if let Some(ns) = self.core.prompts.new_session.take() {
            let why = ns.error.unwrap_or_default();
            self.core.prompts.error = Some(format!(
                "Bubble couldn\u{2019}t create the session \u{201c}{}\u{201d}.\n{why}",
                ns.name
            ));
        }
    }

    /// A native alert was answered: the same as pressing that button in the prompt.
    fn alert_answer(&mut self, d: Dialog, i: usize, intents: &mut Vec<Intent>) {
        self.dialog = Some(d);
        match d {
            Dialog::Error => {
                if i == 1
                    && let Some(text) = &self.core.prompts.error
                    && let Ok(mut c) = arboard::Clipboard::new()
                {
                    let _ = c.set_text(text.clone());
                }
                self.core.prompts.error = None;
            }
            Dialog::Recover => self.dialog_row(0, i, intents),
            Dialog::SetupClose => {
                self.setup_close_ask = false;
                match i {
                    0 => self.close_setup(true, intents),
                    2 => self.close_setup(false, intents),
                    _ => {}
                }
            }
            Dialog::Start => match i {
                0 => self
                    .core
                    .begin_new_session(String::new(), AfterCreate::Empty),
                1 => self.core.files.request(Purpose::Open),
                _ => self.quit_after_apply(),
            },
            Dialog::AudioFiles => {
                self.ask_audio = false;
                if let Some(ns) = &mut self.core.prompts.new_session {
                    ns.convert_to_wav = i == 1;
                    self.create_new_session(intents);
                }
            }
            Dialog::Relink => {
                // Locate… opens the Open panel for the first missing file; whatever is
                // still missing after it stays in the status line (click to come back).
                self.relink = false;
                let snap = self.core.snapshot();
                if i == 0
                    && let Some(a) = snap.missing.first()
                {
                    let file = snap
                        .project
                        .asset(*a)
                        .map(|x| {
                            std::path::Path::new(&x.path)
                                .file_name()
                                .map_or(x.path.clone(), |n| n.to_string_lossy().to_string())
                        })
                        .unwrap_or_default();
                    self.core.files.request_titled(
                        Purpose::Relink(*a),
                        format!("Locate \u{201c}{file}\u{201d}"),
                    );
                }
            }
            _ => self.dialog_button(i, intents),
        }
    }

    fn dialog_button(&mut self, idx: usize, intents: &mut Vec<Intent>) {
        let Some(d) = self.dialog else { return };
        let snap = self.core.snapshot();
        let p = &mut self.core.prompts;
        match (d, idx) {
            (Dialog::Error, _) => p.error = None,
            (Dialog::ConfirmClose, 0) => {
                self.confirm_close = false;
                if snap.path.is_some() {
                    intents.push(Intent::Save);
                    self.quit_after_apply();
                } else {
                    // Untitled: Save As first; quitting is left to the operator.
                    self.core.save_or_save_as(&snap, intents);
                }
            }
            (Dialog::ConfirmClose, 2) => {
                self.confirm_close = false;
                self.core.discard_on_exit = true;
                self.quit_after_apply();
            }
            (Dialog::ConfirmClose, _) => self.confirm_close = false,
            (Dialog::ConfirmDiscard, i) => {
                let action = p.confirm_discard.take();
                if let Some(action) = action {
                    match i {
                        0 => {
                            self.core.save_or_save_as(&snap, intents);
                            self.core.perform(action, intents);
                        }
                        2 => self.core.perform(action, intents),
                        _ => {}
                    }
                }
            }
            (Dialog::ImportChoice, i) => {
                if let Some(path) = p.import_choice.take() {
                    match i {
                        0 => self.core.import_into_current(path),
                        2 => self
                            .core
                            .request(PendingAction::Import(path), &snap, intents),
                        _ => {}
                    }
                }
            }
            // The Save panel answers these (save_chosen, alert_answer).
            (Dialog::NewSession | Dialog::AudioFiles | Dialog::Start | Dialog::SetupClose, _) => {}
            (Dialog::Relink, _) => self.relink = false,
            (Dialog::DeletePage, i) => {
                if let Some(p) = self.delete_page.take()
                    && i == 0
                {
                    intents.push(Intent::RemovePage(p));
                }
            }
            (Dialog::Recover, _) => {}
        }
    }

    fn dialog_row(&mut self, row: usize, action: usize, _intents: &mut Vec<Intent>) {
        match self.dialog {
            Some(Dialog::Recover) => {
                let Some((file, project)) = self.core.prompts.recover.get(row).cloned() else {
                    return;
                };
                if action == 0 {
                    match bubble_project::io::load(file.as_path()) {
                        Ok(p) => {
                            let mut c = self.lock();
                            c.replace_project(p, project.clone());
                            c.set_dirty();
                        }
                        Err(e) => self.core.prompts.error = Some(format!("Could not restore: {e}")),
                    }
                    self.selected = None;
                } else {
                    let _ = std::fs::remove_file(&file);
                }
                self.core.prompts.recover.remove(row);
                // A restored untitled recovery file is no longer needed once loaded.
                if action == 0 && project.is_none() {
                    let _ = std::fs::remove_file(&file);
                }
            }
            Some(Dialog::Relink) => {
                let snap = self.core.snapshot();
                if let Some(a) = snap.missing.get(row) {
                    self.files.request(Purpose::Relink(*a));
                }
            }
            _ => {}
        }
    }

    /// Quits once this callback's intents (a save) are applied.
    fn quit_after_apply(&mut self) {
        self.allow_close = true;
        let w = self.ui.clone();
        slint::Timer::single_shot(Duration::from_millis(1), move || {
            if let Some(ui) = w.upgrade() {
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::CloseRequested);
            }
        });
    }

    // ---- the window ----

    fn track_place(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if let Some(p) = window::place(&ui)
            && self.place.update(&mut self.core, p)
        {
            self.core.save_prefs();
        }
    }

    /// Repaints the whole window twice. After a minimise and restore the software
    /// renderer otherwise kept stale areas (seen on Windows with the prototype).
    fn full_redraw(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_redraw_nonce(ui.get_redraw_nonce() + 1);
        let w = self.ui.clone();
        slint::Timer::single_shot(Duration::from_millis(60), move || {
            if let Some(ui) = w.upgrade() {
                ui.set_redraw_nonce(ui.get_redraw_nonce() + 1);
            }
        });
    }

    /// Files dropped from Finder or Explorer: a session opens (or imports); audio is
    /// assigned from the cell under the pointer, or the first empty cell.
    fn finish_drop(&mut self, intents: &mut Vec<Intent>) {
        self.drag_hover = false;
        let dropped = std::mem::take(&mut self.drops);
        if dropped.is_empty() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        let snap = self.core.snapshot();
        let ext_in = |p: &PathBuf, list: &[&str]| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| list.contains(&e.to_ascii_lowercase().as_str()))
        };
        if let Some(p) = dropped.iter().find(|p| ext_in(p, OPEN_EXTS)) {
            self.core.open_any(p.clone(), &snap, intents);
            return;
        }
        let mut files: Vec<PathBuf> = dropped
            .into_iter()
            .filter(|p| ext_in(p, AUDIO_EXTS))
            .collect();
        files.sort();
        if files.is_empty() {
            return;
        }
        let at = window::pointer_in_window(&ui);
        let target = at
            .map(|(x, y)| ui.invoke_cell_at(x, y))
            .and_then(|idx| self.cell_pos(idx));
        log_info(&format!(
            "{} file(s) dropped at {at:?}: cell {target:?}",
            files.len()
        ));
        match target.or_else(|| first_empty_cell(&snap.project, snap.page)) {
            Some((p, r, c)) => self.core.assign_into_session(p, r, c, files),
            None => self.core.prompts.error = Some("This page is full.".into()),
        }
    }

    fn on_exit(&mut self) {
        if self.exited {
            return;
        }
        self.exited = true;
        if let Some(ui) = self.ui.upgrade()
            && let Some(p) = window::place(&ui)
        {
            self.place.update(&mut self.core, p);
        }
        if self.place.pending() {
            self.core.save_prefs();
        }
        if self.core.discard_on_exit {
            self.core.remove_autosave();
        } else {
            self.core.write_autosave();
        }
    }
}

/// Replaces only the rows that changed (all of them if the length changed).
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, r) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&r) {
            model.set_row_data(i, r);
        }
    }
}
