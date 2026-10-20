//! Renders the window/Dock icon from the vector logo at build time, so the SVG in
//! `assets/` is the only source (no bitmap in the repository).

use resvg::{tiny_skia, usvg};

fn main() {
    let svg = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/logo.svg");
    println!("cargo:rerun-if-changed={svg}");
    let data = std::fs::read(svg).expect("assets/logo.svg");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("valid SVG");
    let n = 256u32;
    let mut pixmap = tiny_skia::Pixmap::new(n, n).expect("pixmap");
    let scale = n as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied alpha; the window icon wants straight RGBA.
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("icon-256.rgba"), rgba).expect("write icon");
}
