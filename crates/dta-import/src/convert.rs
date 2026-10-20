//! Maps a parsed reference session onto a Bubble project and builds
//! the migration report.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bubble_project::resolve::{relative_path, to_portable};
use bubble_project::validate::{GAIN_MAX_DB, GAIN_MIN_DB, MAX_FADE_MS};
use bubble_project::{
    AbsoluteHint, Action, AssetMeta, AssetRef, Binding, BindingSource, Cart, Cell, Curve,
    ExclusiveGroup, Extra, FadeSpec, MidiChannel, MidiMessage, Page, Platform, Project, StopScope,
    Trim, make_asset_ref,
};
use uuid::Uuid;

use crate::dta::{self, HotKey, MidiMsg, Session};
use crate::paths::{DriveMap, PathTranslator, windows_file_name};
use crate::report::{AssetReport, CartReport, CartStatus, Report, Source, Summary, Unsupported};

/// Largest grid a Bubble page supports.
const MAX_GRID: i32 = 16;

/// Marks values whose meaning has not yet been confirmed with a known-value save.
const UNCONFIRMED: &str = "legacy encoding not yet confirmed by a known-value save; check it";

#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Project name; defaults to the session file name.
    pub name: Option<String>,
    /// Directories searched for the reference paths' tails and by file name.
    pub search_roots: Vec<PathBuf>,
    pub drives: DriveMap,
    /// Files extracted from a package: (original path, local file).
    pub packed: Vec<(String, PathBuf)>,
}

/// Identity of the source file, for the report.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub file: String,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConvertError {
    #[error("unsupported grid layout {cols} columns × {rows} rows")]
    Layout { cols: i32, rows: i32 },
}

struct Layout {
    ref_cols: u16,
    per_page: usize,
    cols: u16,
    rows: u16,
    reflowed: bool,
}

impl Layout {
    fn new(cols: i32, rows: i32) -> Result<Layout, ConvertError> {
        let err = ConvertError::Layout { cols, rows };
        if !(1..=160).contains(&cols) || !(1..=160).contains(&rows) {
            return Err(err);
        }
        let per_page = (cols * rows) as usize;
        if per_page > (MAX_GRID * MAX_GRID) as usize {
            return Err(err);
        }
        if cols <= MAX_GRID && rows <= MAX_GRID {
            return Ok(Layout {
                ref_cols: cols as u16,
                per_page,
                cols: cols as u16,
                rows: rows as u16,
                reflowed: false,
            });
        }
        let new_cols = cols.min(MAX_GRID);
        let new_rows = (per_page as i32 + new_cols - 1) / new_cols;
        Ok(Layout {
            ref_cols: cols as u16,
            per_page,
            cols: new_cols as u16,
            rows: new_rows as u16,
            reflowed: true,
        })
    }

    /// (page, row, col) of a 0-based button index.
    fn position(&self, index: u16) -> (usize, u16, u16) {
        let i = index as usize;
        let slot = (i % self.per_page) as u16;
        let cols = if self.reflowed {
            self.cols
        } else {
            self.ref_cols
        };
        (i / self.per_page, slot / cols, slot % cols)
    }
}

/// WAV format needed to turn data-chunk byte offsets into frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WavFormat {
    sample_rate: u32,
    channels: u16,
    block_align: u32,
    /// Length of the file in frames, when the file itself was read.
    frames: Option<u64>,
}

fn read_wav_format(path: &Path) -> Option<WavFormat> {
    let r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();
    let block_align = u32::from(spec.channels) * u32::from(spec.bits_per_sample).div_ceil(8);
    (block_align > 0).then_some(WavFormat {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        block_align,
        frames: Some(u64::from(r.duration())),
    })
}

/// Guesses the WAV format from `nAvgBytesPerSec` and the channel mask when the file
/// cannot be read: the first sample width giving a standard sample rate wins.
fn guess_wav_format(avg_bytes_per_sec: u32, channel_mask: u16) -> Option<WavFormat> {
    const RATES: [u32; 9] = [8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000];
    let channels: Vec<u16> = match channel_mask.count_ones() {
        0 => vec![2, 1],
        n => vec![n as u16],
    };
    for ch in channels {
        for bytes in [2u32, 3, 4, 1] {
            let block_align = u32::from(ch) * bytes;
            if avg_bytes_per_sec.is_multiple_of(block_align)
                && RATES.contains(&(avg_bytes_per_sec / block_align))
            {
                return Some(WavFormat {
                    sample_rate: avg_bytes_per_sec / block_align,
                    channels: ch,
                    block_align,
                    frames: None,
                });
            }
        }
    }
    None
}

/// Windows virtual-key code → the app's key-code name (egui key names, `KeyA`, `Digit1`).
fn vk_to_code(vk: u16) -> Option<(String, Option<&'static str>)> {
    let named = |s: &str| Some((s.to_string(), None));
    match vk {
        0x41..=0x5A => Some((format!("Key{}", vk as u8 as char), None)),
        0x30..=0x39 => Some((format!("Digit{}", vk - 0x30), None)),
        0x60..=0x69 => Some((
            format!("Digit{}", vk - 0x60),
            Some("numpad key mapped to the matching digit key"),
        )),
        0x70..=0x87 => Some((format!("F{}", vk - 0x6F), None)),
        0xBB => named("Equals"),
        0xBD => named("Minus"),
        0xBC => named("Comma"),
        0xBE => named("Period"),
        0xBA => named("Semicolon"),
        0xBF => named("Slash"),
        0xDC => named("Backslash"),
        0xDB => named("OpenBracket"),
        0xDD => named("CloseBracket"),
        0xC0 => named("Backtick"),
        0xDE => named("Quote"),
        0x20 => named("Space"),
        0x0D => named("Enter"),
        0x09 => named("Tab"),
        0x08 => named("Backspace"),
        0x2D => named("Insert"),
        0x2E => named("Delete"),
        0x24 => named("Home"),
        0x23 => named("End"),
        0x21 => named("PageUp"),
        0x22 => named("PageDown"),
        0x25 => named("Left"),
        0x26 => named("Up"),
        0x27 => named("Right"),
        0x28 => named("Down"),
        _ => None,
    }
}

/// `TShiftState` bits → sorted modifier names.
fn shift_to_modifiers(shift: u8) -> Vec<String> {
    let mut m: Vec<String> = [(1, "Shift"), (2, "Alt"), (4, "Ctrl")]
        .into_iter()
        .filter(|(bit, _)| shift & bit != 0)
        .map(|(_, n)| n.to_string())
        .collect();
    m.sort();
    m
}

fn midi_source(m: &MidiMsg) -> Result<BindingSource, String> {
    let (message, threshold) = match m.status() & 0xF0 {
        0x90 => (MidiMessage::NoteOn, None),
        0x80 => (MidiMessage::NoteOff, None),
        0xB0 => (MidiMessage::Cc, Some(64)),
        0xC0 => (MidiMessage::Program, None),
        other => return Err(format!("MIDI status {other:#04x} has no Bubble equivalent")),
    };
    Ok(BindingSource::Midi {
        device: "*".into(),
        channel: MidiChannel((!m.any_channel()).then_some(m.channel())),
        message,
        number: m.data1(),
        threshold,
    })
}

fn fade_curve(law: u16) -> Option<Curve> {
    match law {
        0 => Some(Curve::Linear),
        1 => Some(Curve::Exponential),
        _ => None,
    }
}

fn clean_text(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn file_stem(windows_path: &str) -> String {
    let name = windows_file_name(windows_path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

struct Converter<'a> {
    project_dir: Option<&'a Path>,
    translator: PathTranslator,
    /// Reference path (lower-case) → asset id, so carts sharing a file share an asset.
    assets_by_ref: HashMap<String, Uuid>,
    asset_found: HashMap<Uuid, bool>,
    wav_formats: HashMap<Uuid, Option<WavFormat>>,
    bindings: Vec<Binding>,
}

/// Converts a parsed session. `project_dir` is where the project will be saved; asset
/// paths are made relative to it.
pub fn convert(
    session: &Session,
    source: &SourceInfo,
    project_dir: Option<&Path>,
    opts: &ImportOptions,
) -> Result<(Project, Report), ConvertError> {
    let layout = Layout::new(session.grid_columns, session.grid_rows)?;
    let name = opts.name.clone().unwrap_or_else(|| {
        let stem = file_stem(&source.file);
        let stem = stem.trim_start_matches('~');
        if stem.is_empty() {
            "Imported session".into()
        } else {
            stem.to_string()
        }
    });
    let mut project = Project::new(clean_text(&name, 200));
    project.pages.clear();
    project.settings.default_fade_out_ms =
        (u32::from(session.options.fade_out_default) * 100).min(MAX_FADE_MS);

    let mut c = Converter {
        project_dir,
        translator: PathTranslator::new(opts.search_roots.clone(), opts.drives.clone())
            .with_packed(&opts.packed),
        assets_by_ref: HashMap::new(),
        asset_found: HashMap::new(),
        wav_formats: HashMap::new(),
        bindings: Vec::new(),
    };

    let mut used: Vec<&dta::Cart> = session.carts.iter().filter(|c| c.is_used()).collect();
    used.sort_by_key(|c| c.index);
    let last_page = used.last().map_or(0, |c| layout.position(c.index).0);
    let n_pages = session.page_names.len().max(last_page + 1).max(1);
    for i in 0..n_pages {
        let name = session
            .page_names
            .get(i)
            .map(|n| clean_text(&n.replace('^', " "), 40))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("Page {}", i + 1));
        project
            .pages
            .push(Page::new(name, layout.rows, layout.cols));
    }

    let mut session_warnings = Vec::new();
    let session_unsupported = Vec::new();
    if layout.reflowed {
        session_warnings.push(format!(
            "reference grid {} × {} exceeds Bubble's 16 × 16; buttons were reflowed row by row into {} × {}",
            session.grid_columns, session.grid_rows, layout.cols, layout.rows
        ));
    }
    // The reference "gain offset" attenuates the whole output (0 or −10 dB) so carts
    // can be raised above 0 dB: exactly the project's master gain. Cart gains stay as stored.
    let master = (-session.gain_offset as f32).clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    project.settings.master_gain_db = master;
    session_warnings.push(format!(
        "reference gain offset {} dB imported as master gain {master} dB; cart gains kept as stored",
        -session.gain_offset
    ));
    if !session.options.hotkeys_on {
        session_warnings.push(
            "hot-keys were switched off in the reference session; imported hot-key bindings are active".into(),
        );
    }

    let mut cart_reports = Vec::new();
    for rc in used {
        let (page, row, col) = layout.position(rc.index);
        let (cart, mut report) = c.cart(rc, &mut project);
        report.page = project.pages[page].name.clone();
        report.row = row;
        report.col = col;
        if page >= session.page_names.len() {
            report.warnings.push(format!(
                "on page {} beyond the session's {} visible pages (hidden in the reference app)",
                page + 1,
                session.page_names.len()
            ));
        }
        project.pages[page].cells.push(Cell { row, col, cart });
        cart_reports.push(report);
    }

    project.bindings = std::mem::take(&mut c.bindings);

    // Session-level features with no Bubble equivalent.
    let o = &session.options;
    let any = |f: &dyn Fn(&dta::Cart) -> bool| session.carts.iter().filter(|c| c.is_used()).any(f);
    let mut discarded = Vec::new();
    let mut add = |used: bool, what: &str| {
        if used {
            discarded.push(what.to_string());
        }
    };
    add(
        any(&|c| c.active_effects != 0),
        "DirectSound FX (echo, flanger, reverb, gargle)",
    );
    add(
        o.link_names.iter().any(|n| !n.is_empty())
            || any(&|c| {
                !c.master_links.is_empty()
                    || !c.play_slave_links.is_empty()
                    || !c.stop_slave_links.is_empty()
            }),
        "Master/slave links (voice-over ducking, auto-pan)",
    );
    add(
        any(&|c| c.pitch != 0 && !(25..=400).contains(&c.pitch)),
        "Speed outside 25–400 %",
    );
    add(any(&|c| c.play_seq_count != 0), "Embedded WAV play lists");
    let devices: std::collections::HashSet<&str> = session
        .carts
        .iter()
        .filter(|c| c.is_used())
        .map(|c| c.output_device_guid.as_str())
        .collect();
    add(
        devices.len() > 1,
        "Per-cart output devices: carts were given Output A, B, … by the reference's device index; check Audio → Outputs on this machine",
    );
    add(
        any(&|c| c.midi_play_out.is_valid() || c.midi_stop_out.is_valid()),
        "MIDI out on play/stop",
    );
    add(o.use_gpi_in, "GPI / joystick triggers");
    add(o.use_pbus, "PBus control");
    add(o.use_udp_midi || o.use_tcp_midi, "Network MIDI (UDP/TCP)");
    add(any(&|c| !c.button_image.is_empty()), "Button images");

    let carts_imported = cart_reports.len();
    let assets_missing = c.asset_found.values().filter(|f| !**f).count();
    let assets_resolved = c.asset_found.len() - assets_missing;

    let mut manual = Vec::new();
    if assets_missing > 0 {
        manual.push(format!(
            "Relink {assets_missing} missing file(s) (File ▸ Relink Missing Files…)"
        ));
    }
    let midi_bindings = project
        .bindings
        .iter()
        .any(|b| matches!(b.source, BindingSource::Midi { .. }));
    if midi_bindings {
        manual.push("Choose the MIDI input device (MIDI devices are per machine and were not in the session)".into());
    }
    if cart_reports
        .iter()
        .any(|r| r.warnings.iter().any(|w| w.contains(UNCONFIRMED)))
    {
        manual.push(
            "Check the carts flagged as unconfirmed (hot-keys, MIDI, groups, stop/next behaviour)"
                .into(),
        );
    }
    if !discarded.is_empty() {
        manual.push("Review discarded features and recreate what is needed".into());
    }

    let settings_unsupported = session_unsupported.len()
        + cart_reports
            .iter()
            .map(|r| r.unsupported.len())
            .sum::<usize>();
    let report = Report {
        source: Source {
            file: source.file.clone(),
            format_version: session.format_version,
            trailer_ok: session.trailer_ok,
            sha256: source.sha256.clone(),
        },
        summary: Summary {
            slots_total: session.carts.len(),
            carts_imported,
            assets_resolved,
            assets_missing,
            settings_unsupported,
            features_discarded: discarded.len(),
            manual_actions: manual.len(),
        },
        carts: cart_reports,
        session_unsupported,
        session_warnings,
        discarded_features: discarded,
        manual_actions: manual,
    };
    Ok((project, report))
}

impl Converter<'_> {
    fn cart(&mut self, rc: &dta::Cart, project: &mut Project) -> (Cart, CartReport) {
        let mut cart = Cart::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut unsupported: Vec<Unsupported> = Vec::new();
        let unconfirmed =
            |w: &mut Vec<String>, what: &str| w.push(format!("{what}: {UNCONFIRMED}"));

        // Asset.
        let mut asset_report = None;
        let mut wav = None;
        if let Some(ref_path) = rc.source_path() {
            let (id, rep) = self.asset(ref_path, project, &mut warnings);
            cart.asset = Some(id);
            wav = self.wav_formats.get(&id).copied().flatten();
            asset_report = Some(rep);
            for other in [
                &rc.original_local_filename,
                &rc.remote_filename,
                &rc.original_remote_filename,
            ] {
                let other = other.trim();
                if !other.is_empty() && !other.eq_ignore_ascii_case(ref_path) {
                    warnings.push(format!("original source file not imported: {other}"));
                }
            }
        }

        // Label, colour, notes.
        cart.label = clean_text(&rc.track_name, 200);
        if cart.label.is_empty()
            && let Some(p) = rc.source_path()
        {
            cart.label = clean_text(&file_stem(p), 200);
        }
        match rc.panel_colour.to_hex() {
            Some(hex) => cart.color = Some(hex),
            None => unsupported.push(Unsupported {
                field: "panel_colour".into(),
                value: rc.panel_colour.to_string(),
                action: "dropped (Windows system colour)".into(),
            }),
        }
        cart.notes = clean_text(&rc.comment, bubble_project::validate::MAX_TEXT);

        // Level.
        let gain = rc.preset_gain_db as f32;
        cart.gain_db = gain.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
        if cart.gain_db != gain {
            warnings.push(format!("gain {gain} dB clamped to {} dB", cart.gain_db));
        }
        cart.muted = rc.muted;
        if rc.muted {
            unconfirmed(&mut warnings, "muted");
        }
        if rc.pan.is_finite() && rc.pan != 0.0 {
            let pan = if rc.pan.abs() <= 1.0 {
                rc.pan
            } else {
                rc.pan / 100.0
            };
            cart.pan = pan.clamp(-1.0, 1.0);
            unconfirmed(
                &mut warnings,
                &format!("pan {} imported as {}", rc.pan, cart.pan),
            );
        }

        // Playback.
        cart.looping = rc.looped;
        self.trim(rc, wav, &mut cart, &mut warnings, &mut unsupported);
        let fade = |ds: i32, law: u16, which: &str, w: &mut Vec<String>| {
            let ms = (ds.max(0) as u32 * 100).min(MAX_FADE_MS);
            let curve = fade_curve(law).unwrap_or_else(|| {
                w.push(format!(
                    "unknown {which} law {law}; using the default curve"
                ));
                Curve::EqualPower
            });
            FadeSpec {
                curve,
                ..FadeSpec::ms(ms)
            }
        };
        if rc.fade_in_ds > 0 {
            cart.fade_in = Some(fade(
                rc.fade_in_ds,
                rc.fade_in_law,
                "fade-in",
                &mut warnings,
            ));
        }
        cart.fade_out = Some(fade(
            rc.fade_out_ds,
            rc.fade_out_law,
            "fade-out",
            &mut warnings,
        ));

        // Behaviour.
        cart.on_play.stop = if rc.stop_all_on_play {
            StopScope::All
        } else if rc.stop_page_on_play {
            StopScope::Page
        } else {
            StopScope::None
        };
        if cart.on_play.stop != StopScope::None {
            unconfirmed(&mut warnings, "stop-on-play");
        }
        if rc.stop_row_on_play {
            unsupported.push(Unsupported::dropped("stop_row_on_play", true));
        }
        if rc.play_next {
            cart.play_next = Some("next_cell".into());
            unconfirmed(&mut warnings, "play next");
        }
        cart.hold_to_play = rc.mouse_up_stop;
        if rc.mouse_up_stop {
            unconfirmed(&mut warnings, "stop on mouse-up imported as hold-to-play");
        }
        match rc.play_group {
            0 => {}
            n @ 1..=25 => {
                // Reference group n becomes the project's n-th exclusive group (G n).
                while project.groups.len() < n as usize {
                    project.groups.push(ExclusiveGroup {
                        id: Uuid::new_v4(),
                        name: String::new(),
                        color: None,
                        extra: Extra::new(),
                    });
                }
                cart.group = Some(project.groups[n as usize - 1].id);
                unconfirmed(&mut warnings, &format!("play group {n}"));
            }
            n => unsupported.push(Unsupported::dropped("play_group", n)),
        }

        // Triggers.
        let hk = if rc.hotkey.is_set() {
            rc.hotkey
        } else {
            rc.locked_hotkey
        };
        if hk.is_set() {
            self.hotkey(cart.id, hk, rc.hotkey_on, &mut warnings, &mut unsupported);
        }
        let pick = |a: MidiMsg, b: MidiMsg| {
            if a.is_valid() {
                Some(a)
            } else {
                Some(b).filter(MidiMsg::is_valid)
            }
        };
        if let Some(m) = pick(rc.midi_play_in, rc.locked_midi_play_in) {
            self.midi(
                cart.id,
                m,
                Action::Play,
                "midi_play_in",
                &mut warnings,
                &mut unsupported,
            );
        }
        if let Some(m) = pick(rc.midi_stop_in, rc.locked_midi_stop_in) {
            self.midi(
                cart.id,
                m,
                Action::Stop,
                "midi_stop_in",
                &mut warnings,
                &mut unsupported,
            );
        }
        for (field, m) in [
            (
                "midi_play_out",
                pick(rc.midi_play_out, rc.locked_midi_play_out),
            ),
            (
                "midi_stop_out",
                pick(rc.midi_stop_out, rc.locked_midi_stop_out),
            ),
        ] {
            if let Some(m) = m {
                unsupported.push(Unsupported::dropped(field, m.hex()));
            }
        }

        // Output: the reference's device index becomes the output letter.
        if rc.output_device_guid.chars().any(|c| c != '0' && c != '-') {
            match usize::try_from(rc.output_device_index)
                .ok()
                .filter(|i| *i < bubble_project::MAX_OUTPUTS)
            {
                Some(i) => {
                    cart.output = bubble_project::output_letter(i);
                    warnings.push(format!(
                        "output device {} (index {i}) → Output {}: {UNCONFIRMED}",
                        rc.output_device_guid, cart.output
                    ));
                }
                None => warnings.push(format!(
                    "output device {} (index {}) has no output letter; Output A used",
                    rc.output_device_guid, rc.output_device_index
                )),
            }
        }
        // Speed (25–400 %, 0 = off).
        if (25..=400).contains(&rc.pitch) {
            cart.speed = rc.pitch as f32 / 100.0;
            if rc.pitch != 100 {
                warnings.push(format!("speed {} %: {UNCONFIRMED}", rc.pitch));
            }
        }

        // Settings with no Bubble equivalent.
        let mut drop_if = |cond: bool, field: &str, value: String| {
            if cond {
                unsupported.push(Unsupported::dropped(field, value));
            }
        };
        drop_if(
            rc.active_effects != 0,
            "effects",
            format!("{:#x}", rc.active_effects),
        );
        drop_if(
            rc.pitch != 0 && !(25..=400).contains(&rc.pitch),
            "pitch",
            rc.pitch.to_string(),
        );
        drop_if(
            rc.play_delay_max != 0,
            "play_delay_max",
            rc.play_delay_max.to_string(),
        );
        drop_if(
            rc.slow_down_time != 0,
            "slow_down_time",
            rc.slow_down_time.to_string(),
        );
        drop_if(
            rc.play_seq_count != 0,
            "play_sequence",
            format!("{} entries", rc.play_seq_count),
        );
        drop_if(
            rc.alternate_pan != 0,
            "alternate_pan",
            rc.alternate_pan.to_string(),
        );
        drop_if(
            rc.left_to_both || rc.right_to_both || rc.left_phase || rc.right_phase,
            "channel_patch",
            format!(
                "L→both {}, R→both {}, L phase {}, R phase {}",
                rc.left_to_both, rc.right_to_both, rc.left_phase, rc.right_phase
            ),
        );
        drop_if(
            rc.high_pass_filter != 0 || rc.low_pass_filter != 0,
            "filters",
            format!("HP {}, LP {}", rc.high_pass_filter, rc.low_pass_filter),
        );
        drop_if(rc.fast_midi_mute, "fast_midi_mute", "true".into());
        let links: Vec<String> = [
            ("master", &rc.master_links),
            ("play slave", &rc.play_slave_links),
            ("stop slave", &rc.stop_slave_links),
        ]
        .into_iter()
        .filter(|(_, ids)| !ids.is_empty())
        .map(|(k, ids)| format!("{k} of {ids:?}"))
        .collect();
        drop_if(!links.is_empty(), "links", links.join("; "));
        drop_if(
            !rc.button_image.is_empty(),
            "button_image",
            rc.button_image.clone(),
        );

        let status = match (&asset_report, cart.asset.map(|a| self.asset_found[&a])) {
            (None, _) => CartStatus::ImportedNoAsset,
            (Some(_), Some(true)) => CartStatus::Imported,
            _ => CartStatus::ImportedMissingAsset,
        };
        let report = CartReport {
            ref_button: rc.button(),
            new_cart_id: cart.id,
            page: String::new(),
            row: 0,
            col: 0,
            label: cart.label.clone(),
            status,
            asset: asset_report,
            unsupported,
            warnings,
        };
        (cart, report)
    }

    fn asset(
        &mut self,
        ref_path: &str,
        project: &mut Project,
        warnings: &mut Vec<String>,
    ) -> (Uuid, AssetReport) {
        let located = self.translator.locate(ref_path);
        let mut rep = AssetReport {
            ref_path: ref_path.to_string(),
            resolved: located.found.as_ref().map(|p| p.display().to_string()),
            via: located.via.map(str::to_string),
            tried: located
                .tried
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        };
        let key = ref_path.to_lowercase();
        if let Some(id) = self.assets_by_ref.get(&key) {
            return (*id, rep);
        }

        let mut asset = None;
        if let Some(found) = &located.found {
            match make_asset_ref(self.project_dir, found) {
                Ok(mut a) => {
                    let fmt = read_wav_format(found);
                    if let Some(f) = fmt {
                        a.meta = Some(AssetMeta {
                            sample_rate: f.sample_rate,
                            channels: f.channels,
                            frames: f.frames.unwrap_or_default(),
                            ..AssetMeta::default()
                        });
                    }
                    self.wav_formats.insert(a.id, fmt);
                    asset = Some(a);
                }
                Err(e) => {
                    warnings.push(format!("cannot read {}: {e}", found.display()));
                    rep.resolved = None;
                    rep.via = None;
                    rep.tried.push(found.display().to_string());
                }
            }
        }
        let found = asset.is_some();
        let asset = asset.unwrap_or_else(|| self.missing_asset(ref_path));
        let id = asset.id;
        self.assets_by_ref.insert(key, id);
        self.asset_found.insert(id, found);
        project.assets.push(asset);
        (id, rep)
    }

    /// Asset for a file that was not found: the drive-mapped location (or the bare file
    /// name) as the path, and the original Windows path as `absolute_hint`.
    fn missing_asset(&self, ref_path: &str) -> AssetRef {
        let mapped = self
            .translator
            .mapped(ref_path)
            .and_then(|p| std::path::absolute(p).ok());
        let path = match (
            mapped,
            self.project_dir.and_then(|d| std::path::absolute(d).ok()),
        ) {
            (Some(m), Some(d)) => relative_path(&m, &d).unwrap_or_else(|| to_portable(&m)),
            (Some(m), None) => to_portable(&m),
            _ => windows_file_name(ref_path).to_string(),
        };
        AssetRef {
            id: Uuid::new_v4(),
            path,
            absolute_hint: Some(AbsoluteHint {
                platform: Platform::Windows,
                path: ref_path.to_string(),
            }),
            fingerprint: None,
            meta: None,
            extra: Extra::new(),
        }
    }

    fn trim(
        &self,
        rc: &dta::Cart,
        file: Option<WavFormat>,
        cart: &mut Cart,
        warnings: &mut Vec<String>,
        unsupported: &mut Vec<Unsupported>,
    ) {
        if cart.asset.is_none() || (rc.trim_start_bytes == 0 && rc.trim_end_bytes == 0) {
            return;
        }
        let value = format!("{}–{} bytes", rc.trim_start_bytes, rc.trim_end_bytes);
        let fmt = match (file, rc.avg_bytes_per_sec) {
            (Some(f), avg) => {
                if let Some(avg) = avg
                    && avg != f.sample_rate * f.block_align
                {
                    warnings.push(format!(
                        "file format differs from the session's ({avg} bytes/s); trims converted with the file's format"
                    ));
                }
                f
            }
            (None, Some(avg)) => match guess_wav_format(avg, rc.channel_mask) {
                Some(f) => {
                    warnings.push(format!(
                        "trim converted assuming {} Hz, {} ch, {}-bit (file not readable)",
                        f.sample_rate,
                        f.channels,
                        f.block_align / u32::from(f.channels) * 8
                    ));
                    f
                }
                None => {
                    unsupported.push(Unsupported {
                        field: "trim".into(),
                        value,
                        action: format!("dropped (cannot infer the format from {avg} bytes/s)"),
                    });
                    return;
                }
            },
            (None, None) => {
                unsupported.push(Unsupported {
                    field: "trim".into(),
                    value,
                    action: "dropped (no format information)".into(),
                });
                return;
            }
        };
        let ba = u64::from(fmt.block_align);
        let start = rc.trim_start_bytes.max(0) as u64 / ba;
        let mut end = rc.trim_end_bytes.max(0) as u64 / ba;
        if let Some(frames) = fmt.frames {
            if start == 0 && end >= frames {
                return; // untrimmed
            }
            if end > frames {
                warnings.push(format!(
                    "trim out point clamped to the file's {frames} frames"
                ));
                end = frames;
            }
        }
        if end <= start {
            unsupported.push(Unsupported {
                field: "trim".into(),
                value,
                action: "dropped (in point not before out point)".into(),
            });
            return;
        }
        cart.trim = Some(Trim {
            in_frames: start,
            out_frames: end,
        });
    }

    fn hotkey(
        &mut self,
        cart: Uuid,
        hk: HotKey,
        enabled: bool,
        warnings: &mut Vec<String>,
        unsupported: &mut Vec<Unsupported>,
    ) {
        let value = format!("VK {:#04x}, shift {:#04x}", hk.vk, hk.shift);
        if !enabled {
            unsupported.push(Unsupported {
                field: "hotkey".into(),
                value,
                action: "dropped (disabled on this cart)".into(),
            });
            return;
        }
        let Some((key, note)) = vk_to_code(hk.vk) else {
            unsupported.push(Unsupported {
                field: "hotkey".into(),
                value,
                action: "dropped (key has no Bubble equivalent)".into(),
            });
            return;
        };
        if let Some(n) = note {
            warnings.push(format!("hot-key: {n}"));
        }
        if hk.shift & !0x07 != 0 {
            warnings.push(format!(
                "hot-key: ignored shift-state bits {:#04x}",
                hk.shift & !0x07
            ));
        }
        let source = BindingSource::Key {
            key,
            modifiers: shift_to_modifiers(hk.shift),
        };
        warnings.push(format!(
            "hot-key {}: {UNCONFIRMED}",
            bubble_describe(&source)
        ));
        self.bind(source, Action::Play, cart);
    }

    fn midi(
        &mut self,
        cart: Uuid,
        m: MidiMsg,
        action: Action,
        field: &str,
        warnings: &mut Vec<String>,
        unsupported: &mut Vec<Unsupported>,
    ) {
        match midi_source(&m) {
            Ok(source) => {
                if m.all() {
                    warnings.push(format!("{field}: \"All\" flag not carried over"));
                }
                warnings.push(format!(
                    "{field} {}: {UNCONFIRMED}",
                    bubble_describe(&source)
                ));
                self.bind(source, action, cart);
            }
            Err(why) => unsupported.push(Unsupported {
                field: field.into(),
                value: m.hex(),
                action: format!("dropped ({why})"),
            }),
        }
    }

    /// Adds `cart` to the binding with the same source and action (ganged), or a new one.
    fn bind(&mut self, source: BindingSource, action: Action, cart: Uuid) {
        if let Some(b) = self
            .bindings
            .iter_mut()
            .find(|b| b.source == source && b.action == action)
        {
            b.targets.push(cart);
            return;
        }
        self.bindings.push(Binding {
            id: Uuid::new_v4(),
            source,
            action,
            targets: vec![cart],
            policy: None,
            scope: None,
            velocity_to_gain: false,
            extra: Extra::new(),
        });
    }
}

/// Short label for a binding source in report text.
fn bubble_describe(source: &BindingSource) -> String {
    match source {
        BindingSource::Key { key, modifiers } => {
            let mut parts = modifiers.clone();
            parts.push(key.clone());
            parts.join("+")
        }
        BindingSource::Midi {
            channel,
            message,
            number,
            ..
        } => {
            let ch = channel.0.map_or("any".to_string(), |c| c.to_string());
            format!("{message:?} {number} ch {ch}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_positions() {
        let l = Layout::new(5, 3).unwrap();
        assert_eq!(l.position(0), (0, 0, 0));
        assert_eq!(l.position(1), (0, 0, 1));
        assert_eq!(l.position(5), (0, 1, 0));
        assert_eq!(l.position(15), (1, 0, 0));
        assert_eq!(l.position(319), (21, 0, 4));
        let wide = Layout::new(20, 8).unwrap();
        assert!(wide.reflowed);
        assert_eq!((wide.cols, wide.rows), (16, 10));
        assert_eq!(wide.position(20), (0, 1, 4));
        assert!(Layout::new(0, 3).is_err());
        assert!(Layout::new(20, 16).is_err());
    }

    #[test]
    fn format_guess() {
        let f = guess_wav_format(176_400, 3).unwrap();
        assert_eq!((f.sample_rate, f.channels, f.block_align), (44100, 2, 4));
        let f = guess_wav_format(288_000, 3).unwrap();
        assert_eq!((f.sample_rate, f.block_align), (48000, 6));
        assert!(guess_wav_format(12_345, 3).is_none());
    }

    #[test]
    fn keys_and_modifiers() {
        assert_eq!(vk_to_code(0x41).unwrap().0, "KeyA");
        assert_eq!(vk_to_code(0x70).unwrap().0, "F1");
        assert_eq!(vk_to_code(0x7B).unwrap().0, "F12");
        assert_eq!(vk_to_code(0x65).unwrap().0, "Digit5");
        assert_eq!(vk_to_code(0xBB).unwrap().0, "Equals");
        assert!(vk_to_code(0xFF).is_none());
        assert_eq!(shift_to_modifiers(0b101), vec!["Ctrl", "Shift"]);
    }

    #[test]
    fn midi_mapping() {
        let s = midi_source(&MidiMsg([0, 0, 0, 0, 0x91, 60, 127, 0])).unwrap();
        assert_eq!(
            s,
            BindingSource::Midi {
                device: "*".into(),
                channel: MidiChannel(Some(2)),
                message: MidiMessage::NoteOn,
                number: 60,
                threshold: None
            }
        );
        let s = midi_source(&MidiMsg([1, 0, 0, 0, 0xB0, 7, 0, 0])).unwrap();
        assert!(matches!(
            s,
            BindingSource::Midi {
                channel: MidiChannel(None),
                message: MidiMessage::Cc,
                number: 7,
                ..
            }
        ));
        assert!(midi_source(&MidiMsg([0, 0, 0, 0, 0xE0, 0, 0, 0])).is_err());
    }
}
