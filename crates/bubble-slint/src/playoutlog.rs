//! Playout Log: what played this run (`ui/playoutlog.slint`), from the controller's
//! log. All entries or the problems only (refused carts and dropouts); rows can be
//! selected and copied, the whole log exported as CSV, or cleared.

use std::collections::BTreeSet;

use bubble_control::{LogEntry, LogKind, TriggerSource};
use bubble_engine::EndReason;
use bubble_core::Snapshot;
use bubble_core::app::Core;
use slint::{Color, ModelRc, VecModel};

use crate::LogRow;
use crate::model;

pub struct PlayoutLog {
    pub win: crate::PlayoutLogWindow,
    /// 0 all, 1 problems only.
    filter: i32,
    /// The log's running count when last shown, and its length then.
    seen: (u64, usize),
    /// Entries shown, by their place in the whole run (the running count when added).
    ids: Vec<u64>,
    selected: BTreeSet<u64>,
    /// Where a Shift-click range starts.
    anchor: Option<u64>,
    /// The text of the rows shown (time, cart, what happened), for Copy and Export.
    lines: Vec<[String; 5]>,
}

fn problem(k: &LogKind) -> bool {
    matches!(k, LogKind::Rejected(_) | LogKind::Dropout(_))
}

/// "Refused", "File missing"; the first letter of a reason made a capital.
fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// (kind for the row, verb, detail).
fn describe(k: &LogKind) -> (i32, String, String) {
    match k {
        LogKind::Started(src) => (
            0,
            "Started".into(),
            match src {
                TriggerSource::Mouse => "Click or touch",
                TriggerSource::Key => "Hotkey",
                TriggerSource::Midi => "MIDI",
                TriggerSource::PlayNext => "Play Next",
                TriggerSource::Ui => "Menu or Cart Setup",
            }
            .into(),
        ),
        LogKind::Ended(r) => (
            1,
            "Ended".into(),
            match r {
                EndReason::Completed => "Played to end",
                EndReason::Stopped => "Stopped",
                EndReason::Restarted => "Restarted",
                EndReason::Stolen => "Cut short: too many carts playing",
            }
            .into(),
        ),
        LogKind::Ignored => (5, "Ignored".into(), "On retrigger is Ignore".into()),
        LogKind::Rejected(r) => (2, "Refused".into(), capital(r)),
        LogKind::Dropout(what) => (3, "Dropout".into(), what.clone()),
        LogKind::Info(m) if m == "Outro" => (4, m.clone(), String::new()),
        LogKind::Info(m) => (6, m.clone(), String::new()),
    }
}

fn clock(t: std::time::SystemTime) -> String {
    jiff::Timestamp::try_from(t)
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

impl PlayoutLog {
    pub fn new(win: crate::PlayoutLogWindow) -> Self {
        PlayoutLog {
            win,
            filter: 0,
            seen: (u64::MAX, 0),
            ids: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            lines: Vec::new(),
        }
    }

    /// Follows the log; rebuilt only when it has changed.
    pub fn show(&mut self, core: &Core, snap: &Snapshot) {
        let c = core.lock();
        let total = c.log_total();
        let log = c.log();
        if (total, log.len()) == self.seen {
            return;
        }
        self.seen = (total, log.len());
        // Entry i of the log was the (first + i)th added.
        let first = total - log.len() as u64;
        let mut rows = Vec::new();
        self.ids.clear();
        self.lines.clear();
        let problems = log.iter().filter(|e| problem(&e.kind)).count();
        for (i, e) in log.iter().enumerate() {
            if self.filter == 1 && !problem(&e.kind) {
                continue;
            }
            let id = first + i as u64;
            let (row, line) = self.row(e, id, snap);
            rows.push(row);
            self.ids.push(id);
            self.lines.push(line);
        }
        drop(c);
        self.selected.retain(|id| *id >= first);
        let n = log_len_text(self.seen.1, rows.len(), self.filter == 1);
        self.win.set_all_count(self.seen.1 as i32);
        self.win.set_problem_count(problems as i32);
        self.win.set_filter(self.filter);
        self.win.set_footer(n.into());
        self.win.set_any_selected(!self.selected.is_empty());
        let following = self.win.get_following();
        self.win.set_rows(ModelRc::new(VecModel::from(rows)));
        if following {
            self.win.invoke_to_latest();
        }
    }

    fn row(&self, e: &LogEntry, id: u64, snap: &Snapshot) -> (LogRow, [String; 5]) {
        let (kind, verb, detail) = describe(&e.kind);
        let cart = e.cart.and_then(|c| snap.project.cart(c));
        let number = e
            .cart
            .and_then(|c| snap.project.cart_position(c))
            .map(|(p, r, col)| snap.project.cell_number(p, r, col).to_string())
            .unwrap_or_default();
        let rgb = cart
            .and_then(|c| model::parse_rgb(c.color.as_deref()))
            .unwrap_or(model::DEFAULT_CART_COLOR);
        let label = e.label.replace('|', " ");
        let time = clock(e.time);
        let line = [
            time.clone(),
            number.clone(),
            label.clone(),
            verb.clone(),
            detail.clone(),
        ];
        (
            LogRow {
                time: time.into(),
                swatch: Color::from_rgb_u8(rgb[0], rgb[1], rgb[2]),
                number: if e.cart.is_some() { number.into() } else { "".into() },
                label: label.into(),
                verb: verb.into(),
                detail: detail.into(),
                kind,
                selected: self.selected.contains(&id),
            },
            line,
        )
    }

    /// Shows everything again (a filter or the selection changed).
    fn refresh(&mut self, core: &Core, snap: &Snapshot) {
        self.seen = (u64::MAX, 0);
        self.show(core, snap);
    }

    pub fn row_pressed(&mut self, i: usize, shift: bool, toggle: bool, core: &Core, snap: &Snapshot) {
        let Some(id) = self.ids.get(i).copied() else {
            return;
        };
        match (shift, toggle, self.anchor) {
            (true, _, Some(a)) => {
                let (lo, hi) = (a.min(id), a.max(id));
                self.selected = self.ids.iter().copied().filter(|x| (lo..=hi).contains(x)).collect();
            }
            (_, true, _) => {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.anchor = Some(id);
            }
            _ => {
                self.selected = [id].into();
                self.anchor = Some(id);
            }
        }
        // Choosing rows stops the view following new entries.
        self.win.set_following(false);
        self.refresh(core, snap);
    }

    pub fn set_filter(&mut self, problems: bool, core: &Core, snap: &Snapshot) {
        self.filter = problems.into();
        self.win.set_following(true);
        self.refresh(core, snap);
    }

    /// The selected rows as text, one per line.
    pub fn copy(&self) {
        let text: Vec<String> = self
            .ids
            .iter()
            .zip(&self.lines)
            .filter(|(id, _)| self.selected.contains(id))
            .map(|(_, l)| {
                let what = if l[4].is_empty() {
                    l[3].clone()
                } else {
                    format!("{} · {}", l[3], l[4])
                };
                format!("{}\t{} {}\t{}", l[0], l[1], l[2], what)
                    .replace("\t ", "\t")
            })
            .collect();
        if !text.is_empty()
            && let Ok(mut c) = arboard::Clipboard::new()
        {
            let _ = c.set_text(text.join("\n"));
        }
    }

    /// The whole log (as filtered) as CSV.
    pub fn csv(&self) -> String {
        let mut out = String::from("Time,Cart,Label,Event,Detail\n");
        for l in &self.lines {
            out += &l.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(",");
            out.push('\n');
        }
        out
    }

    pub fn cleared(&mut self, core: &Core, snap: &Snapshot) {
        self.selected.clear();
        self.anchor = None;
        self.win.set_following(true);
        self.refresh(core, snap);
    }
}

/// "112 entries · keeps the last 2,000", or with Problems "6 shown of 112 · …".
fn log_len_text(all: usize, shown: usize, filtered: bool) -> String {
    let n = |v: usize| if v == 1 { "1 entry".to_string() } else { format!("{v} entries") };
    let keep = format!(
        "keeps the last {},{:03}",
        bubble_control::LOG_CAPACITY / 1000,
        bubble_control::LOG_CAPACITY % 1000
    );
    if filtered {
        format!("{shown} shown of {} · {keep}", n(all))
    } else {
        format!("{} · {keep}", n(all))
    }
}
