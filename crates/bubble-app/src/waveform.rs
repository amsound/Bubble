//! Waveform overview and non-destructive trim editing. Only the project's trim
//! points change; audio files are never modified.

use bubble_engine::Overview;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    In,
    Out,
}

/// View state of the editor (per window).
pub struct WaveView {
    /// 1 = whole file visible.
    pub zoom: f64,
    /// Left edge of the view in seconds.
    pub start_s: f64,
    drag: Option<Drag>,
    /// Time under the pointer (seconds from file start), if it is over the waveform.
    pub hover_s: Option<f64>,
    /// Play from here (seconds from file start): set by ⌥/Alt-click or a middle
    /// click, and while dragging with either; taken by the caller.
    pub seek_to: Option<f64>,
    /// Pointer x of the last seek while dragging (seeks at most once per 2 px).
    seek_x: Option<f32>,
}

impl Default for WaveView {
    fn default() -> Self {
        WaveView {
            zoom: 1.0,
            start_s: 0.0,
            drag: None,
            hover_s: None,
            seek_to: None,
            seek_x: None,
        }
    }
}

/// Trim points in seconds, edited in place. Returns true when they changed.
pub struct TrimEdit<'a> {
    pub in_s: &'a mut f64,
    pub out_s: &'a mut f64,
    pub fade_in_s: f64,
    pub fade_out_s: f64,
    /// Playhead in seconds from file start, if playing.
    pub playhead_s: Option<f64>,
}

pub fn waveform_ui(
    ui: &mut egui::Ui,
    peaks: &Overview,
    view: &mut WaveView,
    edit: TrimEdit<'_>,
    height: f32,
) -> bool {
    let dur = peaks.duration_s.max(1e-6);
    view.zoom = view.zoom.clamp(1.0, (dur * 1000.0).max(1.0));
    let span = dur / view.zoom;
    view.start_s = view.start_s.clamp(0.0, (dur - span).max(0.0));

    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    // Colours follow the light or dark theme.
    let dark = ui.visuals().dark_mode;
    let bg = if dark {
        Color32::from_gray(24)
    } else {
        Color32::from_gray(246)
    };
    let grid = if dark {
        Color32::from_gray(60)
    } else {
        Color32::from_gray(205)
    };
    let ruler = if dark {
        Color32::from_gray(150)
    } else {
        Color32::from_gray(110)
    };
    painter.rect_filled(rect, 4.0, bg);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, grid), egui::StrokeKind::Inside);

    // The mapping is fixed for this frame; zoom/scroll changes show on the next one.
    let start_s = view.start_s;
    let t_to_x = move |t: f64| rect.left() + ((t - start_s) / span) as f32 * rect.width();
    let x_to_t = move |x: f32| start_s + ((x - rect.left()) / rect.width()) as f64 * span;
    let mid = rect.center().y;
    let half = rect.height() * 0.45;

    // Zero line and waveform columns.
    painter.line_segment(
        [Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)],
        Stroke::new(1.0, grid),
    );
    let buckets_per_s = peaks.min.len() as f64 / dur;
    let wave_col = if dark {
        Color32::from_rgb(0x5A, 0xB4, 0xFF)
    } else {
        Color32::from_rgb(0x2F, 0x6B, 0xE0)
    };
    let cols = rect.width().max(1.0) as usize;
    for x in 0..cols {
        let t0 = view.start_s + span * x as f64 / cols as f64;
        let t1 = view.start_s + span * (x + 1) as f64 / cols as f64;
        let b0 = ((t0 * buckets_per_s) as usize).min(peaks.min.len() - 1);
        let b1 = ((t1 * buckets_per_s).ceil() as usize).clamp(b0 + 1, peaks.min.len());
        let lo = peaks.min[b0..b1].iter().fold(f32::MAX, |a, v| a.min(*v));
        let hi = peaks.max[b0..b1].iter().fold(f32::MIN, |a, v| a.max(*v));
        let px = rect.left() + x as f32 + 0.5;
        painter.line_segment(
            [
                Pos2::new(px, mid - hi.clamp(-1.0, 1.0) * half),
                Pos2::new(px, mid - lo.clamp(-1.0, 1.0) * half + 0.5),
            ],
            Stroke::new(1.0, wave_col),
        );
    }

    // Time ruler.
    let step = nice_step(span / 8.0);
    let mut t = (view.start_s / step).ceil() * step;
    while t < view.start_s + span {
        let x = t_to_x(t);
        painter.line_segment(
            [
                Pos2::new(x, rect.bottom() - 6.0),
                Pos2::new(x, rect.bottom()),
            ],
            Stroke::new(1.0, ruler),
        );
        painter.text(
            Pos2::new(x + 2.0, rect.bottom() - 7.0),
            Align2::LEFT_BOTTOM,
            fmt_secs(t, step),
            FontId::monospace(10.0),
            ruler,
        );
        t += step;
    }

    // Interaction.
    let in_x = t_to_x(*edit.in_s);
    let out_x = t_to_x(*edit.out_s);
    let mut changed = false;
    let min_len = 0.001;
    // ⌥/Alt-click or middle-click plays from the pointer (drag to keep moving it);
    // plain clicks and drags set In and Out as before.
    let (alt, pressed) = ui.input(|i| {
        (
            i.modifiers.alt,
            i.pointer.button_pressed(egui::PointerButton::Middle)
                || (i.modifiers.alt && i.pointer.button_pressed(egui::PointerButton::Primary)),
        )
    });
    if alt && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let seeking = resp.dragged_by(egui::PointerButton::Middle)
        || (alt && resp.dragged_by(egui::PointerButton::Primary));
    if let Some(p) = resp.hover_pos().or(resp.interact_pointer_pos())
        && ((pressed && resp.hovered())
            || (seeking && view.seek_x.is_none_or(|x| (x - p.x).abs() >= 2.0)))
    {
        view.seek_to = Some(x_to_t(p.x).clamp(0.0, dur));
        view.seek_x = Some(p.x);
    }
    if !seeking && !pressed {
        view.seek_x = None;
    }
    if let Some(pos) = resp.interact_pointer_pos().filter(|_| !alt && !seeking) {
        if resp.drag_started() {
            view.drag = if (pos.x - in_x).abs() < 8.0 {
                Some(Drag::In)
            } else if (pos.x - out_x).abs() < 8.0 {
                Some(Drag::Out)
            } else {
                None
            };
        }
        let t = x_to_t(pos.x).clamp(0.0, dur);
        match view.drag {
            Some(Drag::In) if resp.dragged() => {
                *edit.in_s = t.min(*edit.out_s - min_len).max(0.0);
                changed = true;
            }
            Some(Drag::Out) if resp.dragged() => {
                *edit.out_s = t.max(*edit.in_s + min_len).min(dur);
                changed = true;
            }
            _ => {}
        }
        if resp.clicked() {
            *edit.in_s = t.min(*edit.out_s - min_len).max(0.0);
            changed = true;
        }
        if resp.secondary_clicked() {
            *edit.out_s = t.max(*edit.in_s + min_len).min(dur);
            changed = true;
        }
    }
    if resp.drag_stopped() {
        view.drag = None;
    }
    view.hover_s = resp.hover_pos().map(|p| x_to_t(p.x).clamp(0.0, dur));
    if resp.hovered() {
        let (scroll, pointer) = ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos()));
        if scroll.y != 0.0 {
            let anchor = pointer.map_or(view.start_s + span / 2.0, |p| x_to_t(p.x));
            let factor = (scroll.y as f64 * 0.01).exp();
            let new_zoom = (view.zoom * factor).clamp(1.0, (dur * 1000.0).max(1.0));
            let new_span = dur / new_zoom;
            let frac = (anchor - view.start_s) / span;
            view.zoom = new_zoom;
            view.start_s = (anchor - frac * new_span).clamp(0.0, (dur - new_span).max(0.0));
        }
        if scroll.x != 0.0 {
            view.start_s = (view.start_s - scroll.x as f64 / rect.width() as f64 * span)
                .clamp(0.0, (dur - span).max(0.0));
        }
    }

    // Trimmed-off regions, markers, fade-in ramp, playhead.
    let (in_x, out_x) = (t_to_x(*edit.in_s), t_to_x(*edit.out_s));
    // Outside In/Out is dimmed towards the background.
    let shade = if dark {
        Color32::from_black_alpha(170)
    } else {
        Color32::from_white_alpha(190)
    };
    if in_x > rect.left() {
        painter.rect_filled(
            Rect::from_min_max(
                rect.left_top(),
                Pos2::new(in_x.min(rect.right()), rect.bottom()),
            ),
            0.0,
            shade,
        );
    }
    if out_x < rect.right() {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(out_x.max(rect.left()), rect.top()),
                rect.right_bottom(),
            ),
            0.0,
            shade,
        );
    }
    if edit.fade_in_s > 0.0 {
        let x2 = t_to_x(*edit.in_s + edit.fade_in_s);
        painter.line_segment(
            [
                Pos2::new(in_x, rect.bottom() - 2.0),
                Pos2::new(x2, rect.top() + 2.0),
            ],
            Stroke::new(1.5, Color32::from_rgb(0xFF, 0xC0, 0x40)),
        );
    }
    if edit.fade_out_s > 0.0 {
        let x1 = t_to_x(*edit.out_s - edit.fade_out_s);
        painter.line_segment(
            [
                Pos2::new(x1, rect.top() + 2.0),
                Pos2::new(out_x, rect.bottom() - 2.0),
            ],
            Stroke::new(1.0, Color32::from_rgb(0xFF, 0xC0, 0x40).gamma_multiply(0.5)),
        );
    }
    for (x, label, col) in [
        (in_x, "IN", Color32::from_rgb(0x40, 0xE0, 0x70)),
        (out_x, "OUT", Color32::from_rgb(0xFF, 0x50, 0x50)),
    ] {
        if x >= rect.left() - 1.0 && x <= rect.right() + 1.0 {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(2.0, col),
            );
            painter.text(
                Pos2::new(x + 3.0, rect.top() + 2.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(11.0),
                col,
            );
        }
    }
    if let Some(p) = edit.playhead_s {
        let x = t_to_x(p);
        if x >= rect.left() && x <= rect.right() {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.5, Color32::YELLOW),
            );
        }
    }
    if resp.hovered()
        && let Some(p) = ui.input(|i| i.pointer.hover_pos())
    {
        let near = (p.x - in_x).abs() < 8.0 || (p.x - out_x).abs() < 8.0;
        if near {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    changed
}

fn nice_step(raw: f64) -> f64 {
    let raw = raw.max(1e-4);
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let m = if n < 1.5 {
        1.0
    } else if n < 3.5 {
        2.0
    } else if n < 7.5 {
        5.0
    } else {
        10.0
    };
    m * mag
}

fn fmt_secs(t: f64, step: f64) -> String {
    let m = (t / 60.0).floor();
    let s = t - m * 60.0;
    let decimals = if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else if step >= 0.01 {
        2
    } else {
        3
    };
    if m > 0.0 {
        format!("{}:{:0w$.p$}", m as u32, s, w = decimals + 3, p = decimals)
    } else {
        format!("{:.p$}", s, p = decimals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_steps_are_round() {
        assert_eq!(nice_step(0.7), 0.5);
        assert_eq!(nice_step(2.6), 2.0);
        assert_eq!(fmt_secs(75.5, 0.5), "1:15.5");
    }
}
