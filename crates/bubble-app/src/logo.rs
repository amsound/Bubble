//! The Bubble mark, drawn with vector shapes so it is sharp at any size and display
//! scale. Geometry follows `assets/logo.svg` (1024-unit icon grid); keep them in step.

use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Mesh, Pos2, Rect, Shape, Stroke, Vec2,
};

const TOP: Color32 = Color32::from_rgb(0x4F, 0xD1, 0xFF);
const BOTTOM: Color32 = Color32::from_rgb(0x2F, 0x6B, 0xFF);

/// Circle filled with the logo's diagonal gradient.
fn gradient_circle(painter: &egui::Painter, c: Pos2, r: f32, frame: Rect) {
    let n = 64;
    let mut mesh = Mesh::default();
    let colour = |p: Pos2| {
        // Gradient runs top-left → bottom-right across the icon, as in the SVG.
        let t = (((p.x - frame.left()) * 0.3 + (p.y - frame.top()) * 0.85)
            / (frame.width() * 1.15))
            .clamp(0.0, 1.0);
        lerp(TOP, BOTTOM, t)
    };
    mesh.colored_vertex(c, colour(c));
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let p = c + Vec2::new(a.cos(), a.sin()) * r;
        mesh.colored_vertex(p, colour(p));
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(Shape::mesh(mesh));
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Paints the mark (main bubble with play symbol, trailing bubbles) into `rect`,
/// which should be square.
pub fn paint_mark(painter: &egui::Painter, rect: Rect) {
    // The mark occupies x 196…824, y 192…852 of the 1024 grid; map that box.
    let (x0, y0, span) = (180.0, 180.0, 680.0);
    let s = rect.width().min(rect.height()) / span;
    let origin = rect.center() - Vec2::splat(span * s / 2.0);
    let p = |x: f32, y: f32| origin + Vec2::new((x - x0) * s, (y - y0) * s);
    let frame = Rect::from_min_size(origin, Vec2::splat(span * s));

    gradient_circle(painter, p(226.0, 822.0), 30.0 * s, frame);
    gradient_circle(painter, p(300.0, 736.0), 62.0 * s, frame);
    gradient_circle(painter, p(548.0, 468.0), 276.0 * s, frame);

    // Gloss: an arc on the bubble's upper left.
    let c = p(548.0, 468.0);
    let r = 194.0 * s;
    let (a0, a1) = ((-150.3f32).to_radians(), (-103.5f32).to_radians());
    let pts: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / 24.0;
            c + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect();
    let gloss = Color32::from_white_alpha(140);
    let w = 34.0 * s;
    painter.add(Shape::line(pts.clone(), Stroke::new(w, gloss)));
    for end in [pts[0], pts[pts.len() - 1]] {
        painter.circle_filled(end, w / 2.0, gloss);
    }

    // Play symbol.
    painter.add(Shape::convex_polygon(
        vec![p(500.0, 350.0), p(500.0, 594.0), p(702.0, 474.0)],
        Color32::WHITE,
        Stroke::NONE,
    ));
}

/// The mark with the name beneath, centred in `rect`.
pub fn paint_lockup(painter: &egui::Painter, rect: Rect, text: Color32) {
    let mark = rect.height().min(rect.width()) * 0.62;
    let mrect = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.top() + mark / 2.0),
        Vec2::splat(mark),
    );
    paint_mark(painter, mrect);
    painter.text(
        Pos2::new(rect.center().x, mrect.bottom() + mark * 0.08),
        Align2::CENTER_TOP,
        "Bubble",
        FontId::new(mark * 0.3, FontFamily::Proportional),
        text,
    );
}
