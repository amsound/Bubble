//! `bubble-icon <logo.svg> <out-dir>`: renders the vector logo into the bitmap sizes
//! operating systems need for app icons. The SVG stays the only source.
//!
//! Writes `<out-dir>/Bubble.iconset/icon_<n>x<n>[@2x].png` (for macOS `iconutil`) and
//! `<out-dir>/bubble-<n>.png` for 16–1024 px.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use resvg::{tiny_skia, usvg};

fn render(tree: &usvg::Tree, size: u32) -> Result<Vec<u8>> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).context("pixmap")?;
    let s = tree.size();
    let scale = size as f32 / s.width().max(s.height());
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [svg, out] = args.as_slice() else {
        bail!("usage: bubble-icon <logo.svg> <out-dir>");
    };
    let data = std::fs::read(svg).with_context(|| format!("reading {svg}"))?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())?;
    let out = PathBuf::from(out);
    let set = out.join("Bubble.iconset");
    std::fs::create_dir_all(&set)?;
    for n in [16u32, 32, 128, 256, 512] {
        std::fs::write(set.join(format!("icon_{n}x{n}.png")), render(&tree, n)?)?;
        std::fs::write(
            set.join(format!("icon_{n}x{n}@2x.png")),
            render(&tree, n * 2)?,
        )?;
    }
    for n in [16u32, 32, 48, 64, 128, 256, 512, 1024] {
        std::fs::write(out.join(format!("bubble-{n}.png")), render(&tree, n)?)?;
    }
    println!("{}", out.display());
    Ok(())
}
