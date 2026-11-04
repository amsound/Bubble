//! Playout Log: what played this run (`ui/playoutlog.slint`), from the controller's
//! log. All entries or the problems only (refused carts and dropouts); rows can be
//! selected and copied, the whole log exported as CSV, or cleared.

use std::collections::{BTreeSet, HashMap};
use std::time::SystemTime;

use bubble_control::{LogEntry, LogKind, TriggerSource};
use bubble_core::Snapshot;
use bubble_core::app::Core;
use bubble_engine::EndReason;
use slint::{Color, ModelRc, VecModel};
use uuid::Uuid;

use crate::LogRow;
use crate::model;

pub struct PlayoutLog {
    pub win: crate::PlayoutLogWindow,
    /// 0 all, 1 problems only.
    filter: i32,
    /// The log's running count when last shown, and its length then.
    seen: (u64, usize, u64),
    /// Rows shown, each by its first entry's place in the whole run.
    ids: Vec<u64>,
    selected: BTreeSet<u64>,
    /// Where a Shift-click range starts.
    anchor: Option<u64>,
    /// The rows' text (started, cart number, label, ended, length, what happened), for
    /// Copy and Export.
    lines: Vec<[String; 6]>,
}

/// A row being put together from the log: a play from its start to its end, or an
/// entry on its own.
struct Row {
    id: u64,
    cart: Option<Uuid>,
    label: String,
    start: Option<SystemTime>,
    end: Option<SystemTime>,
    /// What started it, and how it ended (a play), or the verb and detail.
    verb: String,
    detail: String,
    kind: i32,
}

fn problem_kind(k: i32) -> bool {
    k == 2 || k == 3
}

fn started_by(src: TriggerSource) -> &'static str {
    match src {
        TriggerSource::Mouse => "Click or touch",
        TriggerSource::Key => "Hotkey",
        TriggerSource::Midi => "MIDI",
        TriggerSource::PlayNext => "Play Next",
        TriggerSource::Ui => "Menu or Cart Setup",
    }
}

fn ended_by(r: EndReason) -> &'static str {
    match r {
        EndReason::Completed => "Played to end",
        EndReason::Stopped => "Stopped",
        EndReason::Restarted => "Restarted",
        EndReason::Stolen => "Cut short: too many carts playing",
    }
}

/// "First letter capital" for a reason ("file missing" → "File missing").
fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn clock(t: SystemTime) -> String {
    jiff::Timestamp::try_from(t)
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// A play's length: "0:12", "3:05", "1:02:05".
fn length(a: SystemTime, b: SystemTime) -> String {
    let s = b.duration_since(a).map_or(0, |d| d.as_secs());
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The log as rows: each play from its start to its end, paired by the engine's voice
/// (a seek's new voice carries the play on), and everything else on its own.
fn rows_of(log: &std::collections::VecDeque<LogEntry>, first: u64) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // Plays still open: by voice, and by cart (newest last) for those without one yet.
    let mut by_voice: HashMap<u64, usize> = HashMap::new();
    let mut by_cart: HashMap<Uuid, Vec<usize>> = HashMap::new();
    for (i, e) in log.iter().enumerate() {
        let id = first + i as u64;
        let single = |kind: i32, verb: String, detail: String| Row {
            id,
            cart: e.cart,
            label: e.label.clone(),
            start: Some(e.time),
            end: None,
            verb,
            detail,
            kind,
        };
        match &e.kind {
            LogKind::Started(src) => {
                let at = rows.len();
                rows.push(single(7, started_by(*src).into(), String::new()));
                if let Some(v) = e.voice {
                    by_voice.insert(v, at);
                }
                if let Some(c) = e.cart {
                    by_cart.entry(c).or_default().push(at);
                }
            }
            LogKind::Continued => {
                // The cart's newest open play goes on in this voice.
                let open = e
                    .cart
                    .and_then(|c| by_cart.get(&c))
                    .and_then(|v| v.iter().rev().copied().find(|r| rows[*r].kind == 7));
                if let (Some(at), Some(v)) = (open, e.voice) {
                    // The old voice is handed over: its end closes nothing.
                    by_voice.retain(|_, r| *r != at);
                    by_voice.insert(v, at);
                }
            }
            LogKind::Ended(r) => {
                let at = match e.voice {
                    Some(v) => by_voice.remove(&v),
                    None => None,
                };
                match at {
                    Some(at) if rows[at].kind == 7 => {
                        rows[at].end = Some(e.time);
                        rows[at].detail = ended_by(*r).into();
                        rows[at].kind = 0;
                    }
                    // A voice handed on by a seek, or one whose start was cleared.
                    _ if *r == EndReason::Restarted => {}
                    Some(_) => {}
                    None => rows.push(Row {
                        start: None,
                        end: Some(e.time),
                        ..single(0, String::new(), ended_by(*r).into())
                    }),
                }
            }
            LogKind::Ignored => {
                rows.push(single(5, "Ignored".into(), "On retrigger is Ignore".into()))
            }
            LogKind::Rejected(r) => rows.push(single(2, "Refused".into(), capital(r))),
            LogKind::Dropout(what) => rows.push(single(3, "Dropout".into(), what.clone())),
            LogKind::Info(m) if m == "Outro" => rows.push(single(4, "Outro".into(), String::new())),
            LogKind::Info(m) => rows.push(single(6, m.clone(), String::new())),
        }
    }
    rows
}

impl PlayoutLog {
    pub fn new(win: crate::PlayoutLogWindow) -> Self {
        PlayoutLog {
            win,
            filter: 0,
            seen: (u64::MAX, 0, 0),
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
        if (total, log.len(), c.log_rev()) == self.seen {
            return;
        }
        self.seen = (total, log.len(), c.log_rev());
        // Entry i of the log was the (first + i)th added.
        let first = total - log.len() as u64;
        let all = rows_of(log, first);
        drop(c);
        let problems = all.iter().filter(|r| problem_kind(r.kind)).count();
        self.ids.clear();
        self.lines.clear();
        let mut shown = Vec::new();
        for r in all
            .iter()
            .filter(|r| self.filter == 0 || problem_kind(r.kind))
        {
            let (row, line) = self.row(r, snap);
            shown.push(row);
            self.ids.push(r.id);
            self.lines.push(line);
        }
        self.selected.retain(|id| *id >= first);
        let footer = rows_text(all.len(), shown.len(), self.filter == 1);
        self.win.set_all_count(all.len() as i32);
        self.win.set_problem_count(problems as i32);
        self.win.set_filter(self.filter);
        self.win.set_footer(footer.into());
        self.win.set_any_selected(!self.selected.is_empty());
        let following = self.win.get_following();
        self.win.set_rows(ModelRc::new(VecModel::from(shown)));
        if following {
            self.win.invoke_to_latest();
        }
    }

    fn row(&self, r: &Row, snap: &Snapshot) -> (LogRow, [String; 6]) {
        let cart = r.cart.and_then(|c| snap.project.cart(c));
        let number = r
            .cart
            .and_then(|c| snap.project.cart_position(c))
            .map(|(p, row, col)| snap.project.cell_number(p, row, col).to_string())
            .unwrap_or_default();
        let rgb = cart
            .and_then(|c| model::parse_rgb(c.color.as_deref()))
            .unwrap_or(model::DEFAULT_CART_COLOR);
        let label = r.label.replace('|', " ");
        let play = r.kind == 0 || r.kind == 7;
        let started = r.start.map(clock).unwrap_or_else(|| "–".into());
        let ended = match (play, r.end) {
            (true, Some(t)) => clock(t),
            (true, None) => "Playing".into(),
            _ => String::new(),
        };
        let len = match (play, r.start, r.end) {
            (true, Some(a), Some(b)) => length(a, b),
            _ => String::new(),
        };
        let what = if r.verb.is_empty() {
            r.detail.clone()
        } else if r.detail.is_empty() || r.kind == 7 && r.detail == "Playing" {
            r.verb.clone()
        } else {
            format!("{} · {}", r.verb, r.detail)
        };
        (
            LogRow {
                time: started.clone().into(),
                swatch: Color::from_rgb_u8(rgb[0], rgb[1], rgb[2]),
                number: number.clone().into(),
                label: label.clone().into(),
                ended: ended.clone().into(),
                length: len.clone().into(),
                verb: r.verb.clone().into(),
                detail: if r.kind == 7 {
                    String::new()
                } else {
                    r.detail.clone()
                }
                .into(),
                kind: r.kind,
                selected: self.selected.contains(&r.id),
            },
            [started, number, label, ended, len, what],
        )
    }

    /// Shows everything again (a filter or the selection changed).
    fn refresh(&mut self, core: &Core, snap: &Snapshot) {
        self.seen = (u64::MAX, 0, 0);
        self.show(core, snap);
    }

    pub fn row_pressed(
        &mut self,
        i: usize,
        shift: bool,
        toggle: bool,
        core: &Core,
        snap: &Snapshot,
    ) {
        let Some(id) = self.ids.get(i).copied() else {
            return;
        };
        match (shift, toggle, self.anchor) {
            (true, _, Some(a)) => {
                let (lo, hi) = (a.min(id), a.max(id));
                self.selected = self
                    .ids
                    .iter()
                    .copied()
                    .filter(|x| (lo..=hi).contains(x))
                    .collect();
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

    /// The selected rows as text, one per line, tab between columns.
    pub fn copy(&self) {
        let text: Vec<String> = self
            .ids
            .iter()
            .zip(&self.lines)
            .filter(|(id, _)| self.selected.contains(id))
            .map(|(_, l)| {
                let cart = format!("{} {}", l[1], l[2]).trim().to_string();
                [l[0].as_str(), &cart, &l[3], &l[4], &l[5]].join("\t")
            })
            .collect();
        if !text.is_empty()
            && let Ok(mut c) = arboard::Clipboard::new()
        {
            let _ = c.set_text(text.join("\n"));
        }
    }

    /// The log (as filtered) as CSV.
    pub fn csv(&self) -> String {
        let mut out = String::from("Started,Cart,Label,Ended,Length,What happened\n");
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

/// "112 rows · keeps the last 2,000 entries", or with Problems "6 of 112 rows · …".
fn rows_text(all: usize, shown: usize, filtered: bool) -> String {
    let n = |v: usize| {
        if v == 1 {
            "1 row".to_string()
        } else {
            format!("{v} rows")
        }
    };
    let keep = format!(
        "keeps the last {},{:03} entries",
        bubble_control::LOG_CAPACITY / 1000,
        bubble_control::LOG_CAPACITY % 1000
    );
    if filtered {
        format!("{shown} of {} · {keep}", n(all))
    } else {
        format!("{} · {keep}", n(all))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    fn entry(t: u64, cart: Uuid, kind: LogKind, voice: Option<u64>) -> LogEntry {
        LogEntry {
            time: SystemTime::UNIX_EPOCH + Duration::from_secs(t),
            cart: Some(cart),
            label: "Bed".into(),
            kind,
            voice,
        }
    }

    #[test]
    fn plays_pair_by_voice_whatever_order_the_ends_come_in() {
        let c = Uuid::new_v4();
        let started = || LogKind::Started(TriggerSource::Mouse);
        let log: VecDeque<LogEntry> = [
            // Stopped and played again at once: the stop's end arrives after the
            // second start.
            entry(0, c, started(), Some(1)),
            entry(5, c, started(), Some(2)),
            entry(5, c, LogKind::Ended(EndReason::Stopped), Some(1)),
            // A seek: voice 3 carries voice 2's play on, and 2 ends as restarted.
            entry(6, c, LogKind::Continued, Some(3)),
            entry(6, c, LogKind::Ended(EndReason::Restarted), Some(2)),
            entry(9, c, LogKind::Ended(EndReason::Completed), Some(3)),
        ]
        .into();
        let rows = rows_of(&log, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].kind, rows[0].detail.as_str()), (0, "Stopped"));
        assert_eq!(length(rows[0].start.unwrap(), rows[0].end.unwrap()), "0:05");
        assert_eq!(
            (rows[1].kind, rows[1].detail.as_str()),
            (0, "Played to end")
        );
        assert_eq!(length(rows[1].start.unwrap(), rows[1].end.unwrap()), "0:04");
    }
}
