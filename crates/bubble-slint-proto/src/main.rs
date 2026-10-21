//! Bubble main screen in Slint — a throwaway prototype to compare with the egui UI.
//!
//! `bubble-slint-proto [--light|--dark] [--renderer=femtovg|software] [--fps=N] <session.bubble>`
//! `--renderer` (or `SLINT_BACKEND=winit-software`): femtovg (OpenGL) is the default.
//! `--fps`: refresh rate while carts play (default 30).

// No console window behind the app on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use bubble_control::{CartRuntime, CartState as Cs, Controller, MouseButton, display_label};
use bubble_engine::device::{AudioOutput, OutputRequest};
use bubble_project::Project;
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};
use uuid::Uuid;

slint::include_modules!();

const DEFAULT_CART_COLOR: Color = Color::from_rgb_u8(0x3A, 0x6E, 0xA5);
/// UI refresh when idle (engine events only).
const IDLE: Duration = Duration::from_millis(500);

fn main() -> Result<(), slint::PlatformError> {
    let mut path = None;
    let mut theme = None;
    let mut renderer = None;
    let mut fps = 30.0_f64;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--light" => theme = Some(false),
            "--dark" => theme = Some(true),
            _ if a.starts_with("--renderer=") => renderer = Some(a["--renderer=".len()..].to_string()),
            _ if a.starts_with("--fps=") => fps = a["--fps=".len()..].parse().unwrap_or(fps),
            _ => path = Some(PathBuf::from(a)),
        }
    }
    if let Some(r) = &renderer {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name(r.clone())
            .select()?;
    }
    let renderer = renderer
        .or_else(|| std::env::var("SLINT_BACKEND").ok())
        .unwrap_or_else(|| "femtovg".into());

    let mut ctrl = Controller::new(Controller::new_project("Untitled"), None);
    // The stream stops when `_audio` is dropped.
    let _audio = match AudioOutput::open(&OutputRequest::default()) {
        Ok((out, handle)) => {
            ctrl.attach_engine(handle);
            Some(out)
        }
        Err(e) => {
            eprintln!("audio: {e}");
            None
        }
    };
    let mut status = String::new();
    if let Some(p) = &path
        && let Err(e) = ctrl.open(p)
    {
        status = format!("Could not open {}: {e}", p.display());
    }

    // UI refresh while something plays or loads.
    let active = Duration::from_secs_f64(1.0 / fps.clamp(1.0, 120.0));

    let ui = MainWindow::new()?;
    ui.set_title_text(format!("Bubble (Slint prototype, {renderer}, {fps} fps)").into());
    if let Some(dark) = theme {
        ui.global::<Theme>().set_dark(dark);
    }
    ui.set_status(status.into());

    let ctrl = Rc::new(RefCell::new(ctrl));
    let tiles = Rc::new(VecModel::<Tile>::default());
    ui.set_tiles(ModelRc::from(tiles.clone()));
    // Cart ids of the tiles on the current page (None = empty cell).
    let ids: Rc<RefCell<Vec<Option<Uuid>>>> = Rc::default();

    let refresh = {
        let (ui, ctrl, tiles, ids) = (ui.as_weak(), ctrl.clone(), tiles.clone(), ids.clone());
        move || -> bool {
            let Some(ui) = ui.upgrade() else { return false };
            let mut c = ctrl.borrow_mut();
            c.tick();
            let rt = c.runtime();
            let page = c.current_page();
            let project = c.project();
            sync_pages(&ui, project, page);
            let (rows, cols, new_ids, new_tiles) = page_tiles(project, page, &rt);
            ui.set_rows(rows);
            ui.set_cols(cols);
            *ids.borrow_mut() = new_ids;
            // Only rows that changed are replaced, so Slint redraws only those tiles.
            if tiles.row_count() != new_tiles.len() {
                tiles.set_vec(new_tiles);
            } else {
                for (i, t) in new_tiles.into_iter().enumerate() {
                    if tiles.row_data(i).as_ref() != Some(&t) {
                        tiles.set_row_data(i, t);
                    }
                }
            }
            rt.values()
                .any(|r| matches!(r.state, Cs::Playing | Cs::Stopping | Cs::Loading))
        }
    };
    let refresh = Rc::new(refresh);
    refresh();

    let timer = Rc::new(slint::Timer::default());
    {
        let (refresh, weak_timer) = (refresh.clone(), Rc::downgrade(&timer));
        timer.start(slint::TimerMode::Repeated, active, move || {
            let busy = refresh();
            if let Some(t) = weak_timer.upgrade() {
                let want = if busy { active } else { IDLE };
                if t.interval() != want {
                    t.set_interval(want);
                }
            }
        });
    }
    // Input acts at once and refreshes immediately, then the timer runs at the active rate.
    let after_input = {
        let (refresh, timer) = (refresh.clone(), timer.clone());
        move || {
            refresh();
            timer.set_interval(active);
        }
    };

    let cart = {
        let ids = ids.clone();
        move |i: i32| ids.borrow().get(i as usize).copied().flatten()
    };
    ui.on_press({
        let (ctrl, cart, after) = (ctrl.clone(), cart.clone(), after_input.clone());
        move |i| {
            if let Some(id) = cart(i) {
                ctrl.borrow_mut().pointer_down(id, MouseButton::Primary);
                after();
            }
        }
    });
    ui.on_release({
        let (ctrl, cart, after) = (ctrl.clone(), cart.clone(), after_input.clone());
        move |i| {
            if let Some(id) = cart(i) {
                ctrl.borrow_mut().pointer_up(id);
                after();
            }
        }
    });
    ui.on_tap({
        let (ctrl, cart, after) = (ctrl.clone(), cart.clone(), after_input.clone());
        move |i| {
            if let Some(id) = cart(i) {
                let mut c = ctrl.borrow_mut();
                c.pointer_down(id, MouseButton::Primary);
                c.pointer_up(id);
                drop(c);
                after();
            }
        }
    });
    ui.on_menu_action({
        let (ctrl, cart, after, ui) = (ctrl.clone(), cart.clone(), after_input.clone(), ui.as_weak());
        move |i, action| {
            let Some(id) = cart(i) else { return };
            match action.as_str() {
                "play" => {
                    let mut c = ctrl.borrow_mut();
                    c.pointer_down(id, MouseButton::Primary);
                    c.pointer_up(id);
                }
                "stop" => ctrl.borrow_mut().stop_cart(id, true),
                other => {
                    if let Some(ui) = ui.upgrade() {
                        ui.set_status(format!("\"{other}\" is not in the prototype").into());
                    }
                }
            }
            after();
        }
    });
    ui.on_select_page({
        let (ctrl, after) = (ctrl.clone(), after_input.clone());
        move |p| {
            ctrl.borrow_mut().set_current_page(p as usize);
            after();
        }
    });
    ui.on_stop_page({
        let (ctrl, after) = (ctrl.clone(), after_input.clone());
        move || {
            let mut c = ctrl.borrow_mut();
            let p = c.current_page();
            c.stop_page(p);
            drop(c);
            after();
        }
    });
    ui.on_stop_all({
        let (ctrl, after) = (ctrl.clone(), after_input.clone());
        move || {
            ctrl.borrow_mut().stop_all(false);
            after();
        }
    });

    ui.run()
}

fn sync_pages(ui: &MainWindow, project: &Project, current: usize) {
    let names: Vec<PageTab> = project
        .pages
        .iter()
        .map(|p| PageTab {
            name: SharedString::from(p.name.as_str()),
        })
        .collect();
    let model = ui.get_pages();
    if model.row_count() != names.len() || names.iter().enumerate().any(|(i, n)| model.row_data(i).as_ref() != Some(n)) {
        ui.set_pages(ModelRc::new(VecModel::from(names)));
    }
    ui.set_current_page(current as i32);
}

/// Rows, columns, cart ids and tiles for a page, in row-major order.
fn page_tiles(
    project: &Project,
    page: usize,
    rt: &std::collections::HashMap<Uuid, CartRuntime>,
) -> (i32, i32, Vec<Option<Uuid>>, Vec<Tile>) {
    let Some(p) = project.pages.get(page) else {
        return (1, 1, Vec::new(), Vec::new());
    };
    let mut ids = Vec::new();
    let mut tiles = Vec::new();
    for row in 0..p.rows {
        for col in 0..p.cols {
            let number = (row * p.cols + col + 1) as i32;
            let cart = p.cell(row, col).map(|c| &c.cart);
            let Some(cart) = cart.filter(|c| c.asset.is_some() || c.control.is_some()) else {
                ids.push(None);
                tiles.push(Tile {
                    number,
                    state: CartState::Empty,
                    ..Default::default()
                });
                continue;
            };
            let r = rt.get(&cart.id).copied().unwrap_or_default();
            let bg = parse_color(cart.color.as_deref()).unwrap_or(DEFAULT_CART_COLOR);
            let time = if cart.control.is_some() {
                String::new()
            } else {
                match r.state {
                    Cs::Playing | Cs::Stopping => format!("-{}", fmt_clock(r.remaining_s, true)),
                    Cs::Ready => fmt_clock(r.duration_s, true),
                    Cs::Loading => "loading…".into(),
                    Cs::Missing => "MISSING".into(),
                    Cs::Error => "ERROR".into(),
                    Cs::Empty => String::new(),
                }
            };
            ids.push(Some(cart.id));
            tiles.push(Tile {
                has_cart: true,
                number,
                label: display_label(cart, project).into(),
                bg,
                fg: text_color(bg),
                output: format!("Out {}", cart.output).into(),
                time: time.into(),
                // Quantised so a tile is redrawn only when its bar visibly moves.
                progress: (r.progress * 1000.0).round() / 1000.0,
                ending: !r.looping && r.remaining_s < project.settings.ending_warning_s,
                state: match r.state {
                    Cs::Empty => CartState::Empty,
                    Cs::Loading => CartState::Loading,
                    Cs::Missing => CartState::Missing,
                    Cs::Error => CartState::Error,
                    Cs::Ready => CartState::Ready,
                    Cs::Playing => CartState::Playing,
                    Cs::Stopping => CartState::Stopping,
                },
            });
        }
    }
    (p.rows as i32, p.cols as i32, ids, tiles)
}

fn parse_color(s: Option<&str>) -> Option<Color> {
    let v = u32::from_str_radix(s?.strip_prefix('#')?, 16).ok()?;
    Some(Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn text_color(bg: Color) -> Color {
    let l = 0.299 * bg.red() as f32 + 0.587 * bg.green() as f32 + 0.114 * bg.blue() as f32;
    if l > 150.0 {
        Color::from_rgb_u8(0, 0, 0)
    } else {
        Color::from_rgb_u8(255, 255, 255)
    }
}

/// Same as the egui tile clock: MM:SS (H:MM:SS from an hour), rounded up.
fn fmt_clock(s: f32, round_up: bool) -> String {
    if !s.is_finite() {
        return "∞".into();
    }
    let s = s.max(0.0);
    let t = if round_up {
        (s - 1e-3).max(0.0).ceil()
    } else {
        s.floor()
    } as u64;
    let (h, m, sec) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}
