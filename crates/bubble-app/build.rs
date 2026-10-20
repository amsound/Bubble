//! Renders the window/Dock icon from the vector logo at build time, so the SVG in
//! `assets/` is the only source (no bitmap in the repository). For Windows targets it
//! also embeds the logo as the .exe's icon resource.

use std::path::{Path, PathBuf};
use std::process::Command;

use resvg::{tiny_skia, usvg};

fn render(tree: &usvg::Tree, n: u32) -> tiny_skia::Pixmap {
    let mut pixmap = tiny_skia::Pixmap::new(n, n).expect("pixmap");
    let scale = n as f32 / tree.size().width().max(tree.size().height());
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
}

fn main() {
    let svg = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/logo.svg");
    println!("cargo:rerun-if-changed={svg}");
    println!("cargo:rerun-if-changed=build.rs");
    let data = std::fs::read(svg).expect("assets/logo.svg");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("valid SVG");
    let pixmap = render(&tree, 256);
    // tiny-skia stores premultiplied alpha; the window icon wants straight RGBA.
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("icon-256.rgba"), rgba).expect("write icon");

    if std::env::var("TARGET")
        .unwrap_or_default()
        .contains("windows")
    {
        windows_icon(&tree, &out);
    }
}

/// Embeds `bubble.ico` (16–256 px, PNG entries) as the .exe icon. Cosmetic, so never
/// fatal: without a resource compiler the .exe builds with the default icon.
fn windows_icon(tree: &usvg::Tree, out: &Path) {
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes
        .iter()
        .filter_map(|&n| render(tree, n).encode_png().ok())
        .collect();
    if pngs.len() != sizes.len() {
        println!("cargo:warning=icon: could not encode PNGs; building without an icon");
        return;
    }
    let mut ico = vec![0, 0, 1, 0];
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (n, png) in sizes.iter().zip(&pngs) {
        let side = if *n >= 256 { 0 } else { *n as u8 };
        ico.extend_from_slice(&[side, side, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        ico.extend_from_slice(png);
    }
    // The .rc names the icon by a bare file name and is compiled in OUT_DIR, which
    // avoids escaping an absolute path inside the resource script.
    if std::fs::write(out.join("bubble.ico"), &ico).is_err()
        || std::fs::write(out.join("icon.rc"), "1 ICON \"bubble.ico\"\n").is_err()
    {
        println!("cargo:warning=icon: could not write the resource files");
        return;
    }
    let res = out.join("icon.res");
    let ok = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .current_dir(out)
            .status()
            .is_ok_and(|s| s.success())
    };
    let res_s = res.to_string_lossy().to_string();
    let obj = out.join("icon.o");
    let obj_s = obj.to_string_lossy().to_string();
    let linked = if (ok("zig", &["rc", "/fo", &res_s, "icon.rc"])
        || ok("llvm-rc", &["/fo", &res_s, "icon.rc"]))
        && res.exists()
    {
        Some(res)
    } else if ok("windres", &["icon.rc", "-O", "coff", "-o", &obj_s]) && obj.exists() {
        Some(obj)
    } else {
        None
    };
    match linked {
        Some(r) => println!("cargo:rustc-link-arg-bins={}", r.display()),
        None => println!(
            "cargo:warning=icon: no resource compiler (zig rc, llvm-rc, windres); \
             building without an icon"
        ),
    }
}
