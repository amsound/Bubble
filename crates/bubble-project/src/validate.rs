//! Semantic validation: ids unique, references valid,
//! values in range. Invalid projects are rejected as a whole, never half-loaded.

use std::collections::HashSet;

use uuid::Uuid;

use crate::model::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// JSON-path-like location, e.g. `pages[0].cells[3].cart.gain_db`.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

pub const MAX_GRID: u16 = 16;
pub const GAIN_MIN_DB: f32 = -60.0;
pub const GAIN_MAX_DB: f32 = 12.0;
pub const MAX_FADE_MS: u32 = 60_000;
pub const MAX_PLAY_DELAY_MS: u32 = 99_900;
pub const MAX_TEXT: usize = 4096;

pub fn validate(p: &Project) -> Vec<Issue> {
    let mut v = Validator::default();
    v.project(p);
    v.issues
}

#[derive(Default)]
struct Validator {
    issues: Vec<Issue>,
    ids: HashSet<Uuid>,
}

impl Validator {
    fn err(&mut self, path: impl Into<String>, msg: impl Into<String>) {
        self.issues.push(Issue {
            path: path.into(),
            message: msg.into(),
        });
    }

    fn unique(&mut self, id: Uuid, path: &str) {
        if !self.ids.insert(id) {
            self.err(path, format!("duplicate id {id}"));
        }
    }

    fn text(&mut self, s: &str, path: &str, max: usize) {
        if s.len() > max {
            self.err(path, format!("text longer than {max} bytes"));
        }
    }

    fn project(&mut self, p: &Project) {
        if p.format != FORMAT_NAME {
            self.err("format", format!("expected \"{FORMAT_NAME}\""));
        }
        if p.format_version == 0 || p.format_version > CURRENT_FORMAT_VERSION {
            self.err(
                "format_version",
                format!("unsupported version {}", p.format_version),
            );
        }
        self.text(&p.name, "name", 200);
        if p.pages.is_empty() {
            self.err("pages", "a project needs at least one page");
        }
        let d = &p.settings.display;
        if !(0.25..=4.0).contains(&d.label_size) || !(0.25..=4.0).contains(&d.small_size) {
            self.err("settings.display", "text sizes must be 0.25–4");
        }
        if p.settings.default_fade_out_ms > MAX_FADE_MS {
            self.err("settings.default_fade_out_ms", "longer than 60 s");
        }

        let mut assets = HashSet::new();
        for (i, a) in p.assets.iter().enumerate() {
            self.unique(a.id, &format!("assets[{i}].id"));
            assets.insert(a.id);
            if a.path.trim().is_empty() {
                self.err(format!("assets[{i}].path"), "empty path");
            }
            self.text(&a.path, &format!("assets[{i}].path"), MAX_TEXT);
        }
        let mut groups = HashSet::new();
        for (i, g) in p.groups.iter().enumerate() {
            self.unique(g.id, &format!("groups[{i}].id"));
            groups.insert(g.id);
        }

        let mut carts = HashSet::new();
        for (pi, page) in p.pages.iter().enumerate() {
            let pp = format!("pages[{pi}]");
            self.unique(page.id, &format!("{pp}.id"));
            self.text(&page.name, &format!("{pp}.name"), 40);
            if !(1..=MAX_GRID).contains(&page.rows) || !(1..=MAX_GRID).contains(&page.cols) {
                self.err(pp.to_string(), "rows and cols must be 1–16");
            }
            let mut cells = HashSet::new();
            for (ci, cell) in page.cells.iter().enumerate() {
                let cp = format!("{pp}.cells[{ci}]");
                if cell.row >= page.rows || cell.col >= page.cols {
                    self.err(&cp, "cell outside the page grid");
                }
                if !cells.insert((cell.row, cell.col)) {
                    self.err(&cp, "two carts in the same cell");
                }
                let c = &cell.cart;
                self.unique(c.id, &format!("{cp}.cart.id"));
                carts.insert(c.id);
                self.cart(c, &format!("{cp}.cart"), &assets, &groups, p);
            }
        }

        for (pi, page) in p.pages.iter().enumerate() {
            for (ci, cell) in page.cells.iter().enumerate() {
                if let Some(next) = &cell.cart.play_next {
                    let ok = next == "next_cell"
                        || Uuid::parse_str(next).is_ok_and(|id| carts.contains(&id));
                    if !ok {
                        self.err(
                            format!("pages[{pi}].cells[{ci}].cart.play_next"),
                            "unknown cart",
                        );
                    }
                }
            }
        }

        for (bi, b) in p.bindings.iter().enumerate() {
            let bp = format!("bindings[{bi}]");
            self.unique(b.id, &format!("{bp}.id"));
            for t in &b.targets {
                if !carts.contains(t) {
                    self.err(format!("{bp}.targets"), format!("unknown cart {t}"));
                }
            }
            let needs_targets = matches!(
                b.action,
                Action::Play | Action::Stop | Action::Toggle | Action::Pause
            );
            if needs_targets && b.targets.is_empty() {
                self.err(
                    format!("{bp}.targets"),
                    "action needs at least one target cart",
                );
            }
            match &b.source {
                BindingSource::Key { key, .. } => {
                    if key.is_empty() {
                        self.err(format!("{bp}.source.key"), "empty key");
                    }
                }
                BindingSource::Midi {
                    number, threshold, ..
                } => {
                    if *number > 127 {
                        self.err(format!("{bp}.source.number"), "must be 0–127");
                    }
                    if threshold.is_some_and(|t| t > 127) {
                        self.err(format!("{bp}.source.threshold"), "must be 0–127");
                    }
                }
            }
        }
    }

    fn cart(
        &mut self,
        c: &Cart,
        path: &str,
        assets: &HashSet<Uuid>,
        groups: &HashSet<Uuid>,
        p: &Project,
    ) {
        self.text(&c.label, &format!("{path}.label"), 200);
        self.text(&c.notes, &format!("{path}.notes"), MAX_TEXT);
        if let Some(a) = c.asset
            && !assets.contains(&a)
        {
            self.err(format!("{path}.asset"), format!("unknown asset {a}"));
        }
        if let Some(g) = c.group
            && !groups.contains(&g)
        {
            self.err(format!("{path}.group"), format!("unknown group {g}"));
        }
        if crate::model::output_index(&c.output).is_none() {
            self.err(
                format!("{path}.output"),
                format!("output must be a letter A–Z, not {:?}", c.output),
            );
        }
        if !c.gain_db.is_finite() || !(GAIN_MIN_DB..=GAIN_MAX_DB).contains(&c.gain_db) {
            self.err(format!("{path}.gain_db"), "out of range −60…+12 dB");
        }
        if !c.pan.is_finite() || !(-1.0..=1.0).contains(&c.pan) {
            self.err(format!("{path}.pan"), "must be −1…+1");
        }
        if let Some(col) = &c.color
            && !is_hex_color(col)
        {
            self.err(format!("{path}.color"), "must be #RRGGBB");
        }
        for (name, f) in [("fade_in", c.fade_in), ("fade_out", c.fade_out)] {
            if let Some(f) = f
                && f.ms > MAX_FADE_MS
            {
                self.err(format!("{path}.{name}.ms"), "longer than 60 s");
            }
        }
        if !c.speed.is_finite() || !(0.25..=4.0).contains(&c.speed) {
            self.err(format!("{path}.speed"), "must be 0.25–4");
        }
        if c.slowdown_ms > MAX_FADE_MS {
            self.err(format!("{path}.slowdown_ms"), "longer than 60 s");
        }
        if c.speedup_ms > MAX_FADE_MS {
            self.err(format!("{path}.speedup_ms"), "longer than 60 s");
        }
        if c.play_delay_ms > MAX_PLAY_DELAY_MS {
            self.err(format!("{path}.play_delay_ms"), "longer than 99.9 s");
        }
        if let Some(t) = c.trim {
            if t.in_frames >= t.out_frames {
                self.err(format!("{path}.trim"), "in must be before out");
            }
            let frames = c
                .asset
                .and_then(|a| p.asset(a))
                .and_then(|a| a.meta.as_ref())
                .map(|m| m.frames);
            if let Some(frames) = frames
                && t.out_frames > frames
            {
                self.err(format!("{path}.trim.out_frames"), "beyond end of asset");
            }
        }
    }
}

pub fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}
