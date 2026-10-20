//! Typeface choice for cart labels.
//!
//! egui only uses system fonts as glyph fallbacks, so a chosen typeface is loaded from
//! its font file and registered as the `cart-label` family. Only fonts that exist on
//! this machine are offered.

use std::path::Path;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

pub const LABEL_FAMILY: &str = "cart-label";

/// (display name, font file, index within a collection).
type Candidate = (&'static str, &'static str, u32);

#[cfg(target_os = "macos")]
const CANDIDATES: &[Candidate] = &[
    ("Helvetica", "/System/Library/Fonts/Helvetica.ttc", 0),
    ("Helvetica Bold", "/System/Library/Fonts/Helvetica.ttc", 1),
    (
        "Helvetica Neue",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        0,
    ),
    (
        "Helvetica Neue Bold",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        1,
    ),
    ("Avenir Next", "/System/Library/Fonts/Avenir Next.ttc", 7),
    (
        "Avenir Next Demi Bold",
        "/System/Library/Fonts/Avenir Next.ttc",
        2,
    ),
    ("Futura", "/System/Library/Fonts/Supplemental/Futura.ttc", 0),
    (
        "Gill Sans",
        "/System/Library/Fonts/Supplemental/GillSans.ttc",
        0,
    ),
    ("Arial", "/System/Library/Fonts/Supplemental/Arial.ttf", 0),
    (
        "Arial Bold",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        0,
    ),
    (
        "Verdana",
        "/System/Library/Fonts/Supplemental/Verdana.ttf",
        0,
    ),
    ("Menlo", "/System/Library/Fonts/Menlo.ttc", 0),
];

#[cfg(target_os = "windows")]
const CANDIDATES: &[Candidate] = &[
    ("Segoe UI", "C:\\Windows\\Fonts\\segoeui.ttf", 0),
    ("Segoe UI Bold", "C:\\Windows\\Fonts\\segoeuib.ttf", 0),
    ("Arial", "C:\\Windows\\Fonts\\arial.ttf", 0),
    ("Arial Bold", "C:\\Windows\\Fonts\\arialbd.ttf", 0),
    ("Calibri", "C:\\Windows\\Fonts\\calibri.ttf", 0),
    ("Tahoma", "C:\\Windows\\Fonts\\tahoma.ttf", 0),
    ("Verdana", "C:\\Windows\\Fonts\\verdana.ttf", 0),
    ("Consolas", "C:\\Windows\\Fonts\\consola.ttf", 0),
];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const CANDIDATES: &[Candidate] = &[
    (
        "DejaVu Sans",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        0,
    ),
    (
        "DejaVu Sans Bold",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        0,
    ),
    (
        "Liberation Sans",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        0,
    ),
    (
        "Liberation Sans Bold",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        0,
    ),
    (
        "Noto Sans",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        0,
    ),
];

/// Interface fonts in order of preference: the platform's own UI font first.
#[cfg(target_os = "macos")]
const UI_FONTS: &[(&str, u32)] = &[
    ("/System/Library/Fonts/SFNS.ttf", 0),
    ("/System/Library/Fonts/HelveticaNeue.ttc", 0),
];
#[cfg(target_os = "macos")]
const MONO_FONTS: &[(&str, u32)] = &[
    ("/System/Library/Fonts/SFNSMono.ttf", 0),
    ("/System/Library/Fonts/Menlo.ttc", 0),
];
#[cfg(target_os = "windows")]
const UI_FONTS: &[(&str, u32)] = &[("C:\\Windows\\Fonts\\segoeui.ttf", 0)];
#[cfg(target_os = "windows")]
const MONO_FONTS: &[(&str, u32)] = &[("C:\\Windows\\Fonts\\consola.ttf", 0)];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const UI_FONTS: &[(&str, u32)] = &[
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONTS: &[(&str, u32)] = &[("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0)];

/// Loads the first font of `list` that exists, under `key`.
fn load_first(defs: &mut FontDefinitions, key: &str, list: &[(&str, u32)]) -> bool {
    for (path, index) in list {
        if let Ok(bytes) = std::fs::read(path) {
            let mut data = FontData::from_owned(bytes);
            data.index = *index;
            defs.font_data.insert(key.to_string(), Arc::new(data));
            return true;
        }
    }
    false
}

/// Typefaces available on this machine (besides the built-in default).
pub fn available() -> Vec<&'static str> {
    CANDIDATES
        .iter()
        .filter(|(_, path, _)| Path::new(path).is_file())
        .map(|(name, _, _)| *name)
        .collect()
}

/// Tracks which typeface is installed so fonts are only rebuilt on change.
#[derive(Default)]
pub struct FontState {
    applied: Option<Option<String>>,
}

impl FontState {
    /// Installs `name` (or the built-in font) as the cart-label family.
    pub fn apply(&mut self, ctx: &egui::Context, name: Option<&str>) {
        let wanted = name.map(str::to_string);
        if self.applied.as_ref() == Some(&wanted) {
            return;
        }
        self.applied = Some(wanted);
        let mut defs = FontDefinitions::default();
        // The interface uses the system font; egui's own fonts stay as fallbacks for
        // symbols and emoji.
        if load_first(&mut defs, "system-ui", UI_FONTS)
            && let Some(f) = defs.families.get_mut(&FontFamily::Proportional)
        {
            f.insert(0, "system-ui".into());
        }
        if load_first(&mut defs, "system-mono", MONO_FONTS)
            && let Some(f) = defs.families.get_mut(&FontFamily::Monospace)
        {
            f.insert(0, "system-mono".into());
        }
        let fallback = defs
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        let mut family = Vec::new();
        if let Some((n, path, index)) = name.and_then(|n| CANDIDATES.iter().find(|c| c.0 == n))
            && let Ok(bytes) = std::fs::read(path)
        {
            let mut data = FontData::from_owned(bytes);
            data.index = *index;
            defs.font_data.insert(n.to_string(), Arc::new(data));
            family.push(n.to_string());
        }
        family.extend(fallback);
        defs.families
            .insert(FontFamily::Name(LABEL_FAMILY.into()), family);
        ctx.set_fonts(defs);
    }
}

pub fn label_family() -> FontFamily {
    FontFamily::Name(LABEL_FAMILY.into())
}
