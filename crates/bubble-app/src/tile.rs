//! Drawing one cart tile.
//!
//! Layout (all small text scales with `Display::small_size`):
//! ```text
//! ┌──────────────────────────────────────┐
//! │ 12  G1 Drums                [ F1 ]   │  number, exclusive group, triggers
//! │                                      │
//! │            Label text                │  font, size and alignment from Display
//! │                                      │
//! │ > < ⟲ ×0.75 ■ →             -1:23.4  │  trim, loop, speed, stop-others, next; time
//! └▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀────────────────────┘  progress
//! ```

use bubble_control::{CartRuntime, CartState, display_label};
use bubble_project::{Cart, HAlign, Project, VAlign};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2,
};

use crate::fonts::label_family;

pub const DEFAULT_CART_COLOR: Color32 = Color32::from_rgb(0x3A, 0x6E, 0xA5);
pub const EMPTY_COLOR: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x31);

pub struct TileState {
    pub selected: bool,
    pub learning: bool,
    pub drop_hover: bool,
    pub edit_mode: bool,
    /// 1-based position on the page.
    pub number: Option<u16>,
    /// The cart's output device is missing; it plays on the fallback output.
    pub rerouted: bool,
}

pub fn parse_color(s: Option<&str>) -> Option<Color32> {
    let s = s?.strip_prefix('#')?;
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn text_color(bg: Color32) -> Color32 {
    let l = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if l > 150.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

pub use bubble_core::view::{fmt_precise, group_label, tile_text, trigger_names};

pub fn paint_empty(ui: &egui::Ui, rect: Rect, st: &TileState) {
    let painter = ui.painter_at(rect.expand(3.0));
    let radius = CornerRadius::same(6);
    // A touch lighter on a dark background, so empty cells stay visible.
    let empty = if ui.visuals().dark_mode {
        Color32::from_rgb(0x36, 0x3A, 0x40)
    } else {
        EMPTY_COLOR
    };
    painter.rect_filled(rect, radius, empty);
    if st.edit_mode {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "+",
            FontId::proportional(22.0),
            Color32::from_gray(90),
        );
    }
    if st.selected && st.edit_mode {
        painter.rect_stroke(
            rect.expand(2.0),
            radius,
            Stroke::new(2.0, Color32::from_rgb(0x80, 0xC0, 0xFF)),
            StrokeKind::Outside,
        );
    }
    if st.drop_hover {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(3.0, Color32::YELLOW),
            StrokeKind::Inside,
        );
    }
}

pub fn paint_cart(
    ui: &egui::Ui,
    rect: Rect,
    cart: &Cart,
    project: &Project,
    rt: CartRuntime,
    st: &TileState,
) {
    let painter = ui.painter_at(rect.expand(3.0));
    let radius = CornerRadius::same(6);
    let disp = &project.settings.display;
    let mut bg = parse_color(cart.color.as_deref()).unwrap_or(DEFAULT_CART_COLOR);
    if matches!(
        rt.state,
        CartState::Missing | CartState::Error | CartState::Empty
    ) {
        bg = bg.gamma_multiply(0.35);
    }
    painter.rect_filled(rect, radius, bg);
    let fg = text_color(bg);
    let pad = 7.0;
    let small = (11.0 * disp.small_size).clamp(7.0, 40.0);
    let mono_small = FontId::monospace(small);

    let playing = matches!(rt.state, CartState::Playing | CartState::Stopping);

    // ---- top row: number + group (left), triggers (right) ----
    let top_y = rect.top() + 4.0;
    let mut x = rect.left() + pad;
    if let Some(n) = st.number {
        let g = painter.text(
            Pos2::new(x, top_y),
            Align2::LEFT_TOP,
            n.to_string(),
            mono_small.clone(),
            fg.gamma_multiply(0.7),
        );
        x = g.right() + 6.0;
    }
    let mut top_h = small + 2.0;
    if let Some(gl) = group_label(cart, project) {
        let galley = painter.layout_no_wrap(gl, FontId::proportional(small), fg);
        let r = Rect::from_min_size(
            Pos2::new(x, top_y - 1.0),
            galley.size() + Vec2::new(8.0, 2.0),
        );
        painter.rect_filled(r, CornerRadius::same(3), fg.gamma_multiply(0.18));
        painter.galley(r.min + Vec2::new(4.0, 1.0), galley, fg);
        top_h = top_h.max(r.height());
    }
    let triggers = trigger_names(cart, project);
    if !triggers.is_empty() {
        // Prominent "keycap" so the operator can read the hotkey at a glance.
        let text = triggers.join("  ");
        let galley = painter.layout_no_wrap(text, FontId::monospace((small * 1.15).max(9.0)), bg);
        let size = galley.size() + Vec2::new(8.0, 2.0);
        let r = Rect::from_min_size(Pos2::new(rect.right() - pad - size.x, top_y - 1.0), size);
        painter.rect_filled(r, CornerRadius::same(3), fg.gamma_multiply(0.85));
        painter.galley(r.min + Vec2::new(4.0, 1.0), galley, bg);
        top_h = top_h.max(r.height());
    }

    // ---- bottom row: flags (left), time (right) ----
    // Fixed, so nothing moves when a cart starts (the progress track sits below).
    let bottom_y = rect.bottom() - 15.0;
    let text = tile_text(cart, project, &rt);
    let mut flags: Vec<String> = if cart.control.is_some() {
        text.flags
    } else {
        // The output letter first, so it stays in place whatever follows it.
        std::iter::once(text.output).chain(text.flags).collect()
    };
    let time = text.time;
    let time_font =
        FontId::monospace(((rect.height() / 6.0).clamp(11.0, 22.0) * disp.small_size).max(8.0));
    let time_g = painter.text(
        Pos2::new(rect.right() - pad, bottom_y),
        Align2::RIGHT_BOTTOM,
        time,
        time_font,
        fg,
    );
    let flag_font = FontId::proportional(small * 1.15);
    let mut flags_x = rect.left() + pad;
    // A rerouted output is a badge, so it is noticed before the cart is played.
    if st.rerouted && cart.control.is_none() && !flags.is_empty() {
        let out = flags.remove(0);
        let galley = painter.layout_no_wrap(out, flag_font.clone(), Color32::BLACK);
        let size = galley.size() + Vec2::new(8.0, 2.0);
        let r = Rect::from_min_size(Pos2::new(flags_x, bottom_y - size.y), size);
        painter.rect_filled(
            r,
            CornerRadius::same(3),
            Color32::from_rgb(0xFF, 0xB0, 0x20),
        );
        painter.galley(r.min + Vec2::new(4.0, 1.0), galley, Color32::BLACK);
        flags_x = r.right() + 4.0;
    }
    let flags_g = painter.text(
        Pos2::new(flags_x, bottom_y),
        Align2::LEFT_BOTTOM,
        flags.join(" "),
        flag_font,
        fg,
    );
    let bottom_h = time_g.height().max(flags_g.height());

    // ---- label in the remaining area ----
    let area = Rect::from_min_max(
        Pos2::new(rect.left() + pad, rect.top() + 4.0 + top_h + 2.0),
        Pos2::new(rect.right() - pad, bottom_y - bottom_h - 2.0),
    );
    if area.width() > 4.0 && area.height() > 4.0 {
        let size = ((rect.height() / 5.0).clamp(12.0, 26.0) * disp.label_size).clamp(6.0, 120.0);
        let align = match disp.h_align {
            HAlign::Left => Align::LEFT,
            HAlign::Center => Align::Center,
            HAlign::Right => Align::RIGHT,
        };
        let mut job = egui::text::LayoutJob::simple(
            display_label(cart, project),
            FontId::new(size, label_family()),
            fg,
            area.width(),
        );
        job.halign = align;
        let galley = painter.layout_job(job);
        let h = galley.size().y.min(area.height());
        let y = match disp.v_align {
            VAlign::Top => area.top(),
            VAlign::Center => area.center().y - h / 2.0,
            VAlign::Bottom => area.bottom() - h,
        };
        // With a non-left alignment the galley is positioned around its anchor x.
        let x = match disp.h_align {
            HAlign::Left => area.left(),
            HAlign::Center => area.center().x,
            HAlign::Right => area.right(),
        };
        let clip = painter.with_clip_rect(area.intersect(rect));
        clip.galley(Pos2::new(x, y), galley, fg);
    }

    // ---- state: steady yellow border while playing, purple while fading out ----
    let border = match rt.state {
        CartState::Playing => Some(Color32::from_rgb(0xFF, 0xD8, 0x2E)),
        CartState::Stopping => Some(Color32::from_rgb(0xA8, 0x55, 0xF7)),
        _ => None,
    };
    if let Some(col) = border {
        painter.rect_stroke(rect, radius, Stroke::new(5.0, col), StrokeKind::Inside);
    }
    // Progress, drawn last and inside the border so nothing covers it.
    if playing {
        let track = Rect::from_min_max(
            Pos2::new(rect.left() + 5.0, rect.bottom() - 13.0),
            Pos2::new(rect.right() - 5.0, rect.bottom() - 5.0),
        );
        painter.rect_filled(track, CornerRadius::same(2), Color32::from_black_alpha(110));
        let mut bar = track;
        bar.set_right(track.left() + track.width() * rt.progress.clamp(0.0, 1.0));
        let ending = !rt.looping && rt.remaining_s < project.settings.ending_warning_s;
        let col = if ending {
            Color32::from_rgb(0xFF, 0x50, 0x30)
        } else {
            Color32::from_rgb(0x40, 0xFF, 0x80)
        };
        painter.rect_filled(bar, CornerRadius::same(2), col);
    }
    if st.selected && st.edit_mode {
        painter.rect_stroke(
            rect.expand(2.0),
            radius,
            Stroke::new(2.0, Color32::from_rgb(0x80, 0xC0, 0xFF)),
            StrokeKind::Outside,
        );
    }
    if st.learning {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(4.0, Color32::from_rgb(0xFF, 0x40, 0xFF)),
            StrokeKind::Inside,
        );
    }
    if st.drop_hover {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(3.0, Color32::YELLOW),
            StrokeKind::Inside,
        );
    }
}

