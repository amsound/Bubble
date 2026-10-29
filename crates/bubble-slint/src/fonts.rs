//! The system UI font as the system draws it.
//!
//! macOS ships San Francisco as one variable font (SFNS.ttf) whose default optical
//! size is 28, the Display design. Core Text picks the Text design (opsz 17) for
//! interface sizes; Slint draws the default instance, which sets about 13% narrower
//! with Display letterforms. So, once per installed font, Bubble makes static Text
//! instances of the user's own SFNS.ttf (nothing of Apple's is shipped) in its cache
//! folder and hands them to Slint through `SLINT_FONT_PATH`, which puts them first in
//! the default font chain.

/// Prepares the font before Slint starts; returns the family the window should name,
/// or "" when Slint's default chain already holds the right font.
#[cfg(target_os = "macos")]
pub fn prepare() -> &'static str {
    match sf_text() {
        Ok(dir) => {
            // SAFETY: called from `main` before Slint or any other thread starts.
            unsafe { std::env::set_var("SLINT_FONT_PATH", &dir) };
            ""
        }
        Err(e) => {
            bubble_core::log::info(&format!("using the variable system font: {e}"));
            "System Font"
        }
    }
}

/// Windows 11's UI face, Segoe UI Variable (Text optical size), where installed;
/// Windows 10 has only Segoe UI.
#[cfg(not(target_os = "macos"))]
pub fn prepare() -> &'static str {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    if std::path::Path::new(&windir)
        .join("Fonts")
        .join("SegUIVar.ttf")
        .is_file()
    {
        "Segoe UI Variable Text"
    } else {
        "Segoe UI"
    }
}

#[cfg(target_os = "macos")]
const SFNS: &str = "/System/Library/Fonts/SFNS.ttf";

/// The weights Bubble draws: regular, medium, semibold and bold.
#[cfg(target_os = "macos")]
const WEIGHTS: [f32; 4] = [400.0, 500.0, 600.0, 700.0];

/// The folder holding the Text instances, made if they are not there yet.
#[cfg(target_os = "macos")]
fn sf_text() -> Result<std::path::PathBuf, String> {
    use std::time::UNIX_EPOCH;

    let meta = std::fs::metadata(SFNS).map_err(|e| e.to_string())?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let home = std::env::var_os("HOME").ok_or("no home folder")?;
    let root = std::path::Path::new(&home).join("Library/Caches/Bubble/sf-text");
    // A macOS update that changes the font gets new instances.
    let dir = root.join(format!("{}-{stamp}", meta.len()));
    let file = |w: f32| dir.join(format!("sf-text-{w}.ttf"));
    if WEIGHTS.iter().all(|&w| file(w).is_file()) {
        return Ok(dir);
    }
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = std::fs::read(SFNS).map_err(|e| e.to_string())?;
    for w in WEIGHTS {
        let bytes = instance(&data, w)?;
        std::fs::write(file(w), bytes).map_err(|e| e.to_string())?;
    }
    bubble_core::log::info("made the San Francisco Text instances");
    Ok(dir)
}

/// SFNS at optical size 17 and weight `w`, every other axis at its default.
#[cfg(target_os = "macos")]
fn instance(data: &[u8], w: f32) -> Result<Vec<u8>, String> {
    use allsorts::binary::read::ReadScope;
    use allsorts::font_data::FontData;
    use allsorts::tables::variable_fonts::fvar::FvarTable;
    use allsorts::tables::{Fixed, FontTableProvider};
    use allsorts::tag;

    let font = ReadScope::new(data)
        .read::<FontData<'_>>()
        .map_err(|e| e.to_string())?;
    let provider = font.table_provider(0).map_err(|e| e.to_string())?;
    let fvar_data = provider
        .read_table_data(tag::FVAR)
        .map_err(|e| e.to_string())?;
    let fvar = ReadScope::new(&fvar_data)
        .read::<FvarTable<'_>>()
        .map_err(|e| e.to_string())?;
    let user: Vec<Fixed> = fvar
        .axes()
        .map(|a| {
            Fixed::from(match a.axis_tag {
                tag::OPSZ => 17.0,
                tag::WGHT => w,
                _ => f32::from(a.default_value),
            })
        })
        .collect();
    let (bytes, _) =
        allsorts::variations::instance(&provider, &user).map_err(|e| format!("{e:?}"))?;
    Ok(bytes)
}
