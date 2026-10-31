//! Earlier versions of each session, kept in Bubble's own folder so they survive a
//! session folder being moved or deleted, and stay out of cloud sync. One folder per
//! session (by its id), one file per version, named by the time it was made.
//!
//! A session's file is small, so these are generous: the latest versions, enough for
//! hours of work, then one for each of the last two weeks.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use jiff::civil::DateTime;
use uuid::Uuid;

/// The newest versions, all kept.
const RECENT_KEPT: usize = 30;
/// Beyond those, the last version of each of this many days.
const DAYS_KEPT: i32 = 14;

const TIME_NAME: &str = "%Y-%m-%d %H-%M-%S";

/// The folder of a session's versions.
pub fn folder(session: Uuid) -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Bubble", "Bubble")
        .map(|d| d.data_dir().join("Backups").join(session.to_string()))
}

/// Keeps `bytes` as the session's newest version, unless the newest already is the same,
/// and removes those no longer kept. On the file writer's thread.
pub fn keep(dir: &Path, bytes: &[u8], now: DateTime) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if let Some((newest, _)) = versions(dir).first()
        && std::fs::read(newest).is_ok_and(|b| b == bytes)
    {
        return Ok(());
    }
    let name = format!("{}.bubble", now.strftime(TIME_NAME));
    bubble_project::io::write_atomic(&dir.join(name), bytes, false)?;
    let today = now.date();
    for gone in to_remove(&versions(dir), today) {
        let _ = std::fs::remove_file(gone);
    }
    Ok(())
}

/// A session's versions, newest first, with their times.
pub fn versions(dir: &Path) -> Vec<(PathBuf, DateTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(PathBuf, DateTime)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            let time = DateTime::strptime(TIME_NAME, &stem).ok()?;
            Some((path, time))
        })
        .collect();
    found.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    found
}

/// Given versions newest first: all but the newest few and each recent day's last.
fn to_remove(versions: &[(PathBuf, DateTime)], today: jiff::civil::Date) -> Vec<PathBuf> {
    let mut days = HashSet::new();
    versions
        .iter()
        .enumerate()
        .filter_map(|(i, (path, time))| {
            let day = time.date();
            let first_of_day = days.insert(day);
            let recent_day = today
                .since(day)
                .is_ok_and(|span| span.get_days() < DAYS_KEPT);
            (i >= RECENT_KEPT && !(first_of_day && recent_day)).then(|| path.clone())
        })
        .collect()
}

/// "11:27 today", "18:00 yesterday", "14:14 Sun 11 Apr" ("… 2025" from another year).
pub fn label(time: DateTime, today: jiff::civil::Date) -> String {
    let clock = time.strftime("%H:%M");
    let day = time.date();
    if day == today {
        format!("{clock} today")
    } else if today.yesterday().is_ok_and(|y| y == day) {
        format!("{clock} yesterday")
    } else if day.year() == today.year() {
        format!("{clock} {}", time.strftime("%a %-d %b"))
    } else {
        format!("{clock} {}", time.strftime("%a %-d %b %Y"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn labels_read_like_the_time_they_were() {
        let today = date(2026, 4, 13);
        let at = |d: jiff::civil::Date| d.at(11, 27, 0, 0);
        assert_eq!(label(at(today), today), "11:27 today");
        assert_eq!(label(at(date(2026, 4, 12)), today), "11:27 yesterday");
        assert_eq!(label(at(date(2026, 4, 11)), today), "11:27 Sat 11 Apr");
        assert_eq!(label(at(date(2025, 4, 11)), today), "11:27 Fri 11 Apr 2025");
    }

    #[test]
    fn keeps_the_newest_and_one_per_recent_day_and_nothing_twice() {
        let dir = tempfile::tempdir().unwrap();
        let start = date(2026, 4, 1).at(9, 0, 0, 0);
        let mut now = start;
        for i in 0..200 {
            keep(dir.path(), format!("{i}").as_bytes(), now).unwrap();
            now = now.checked_add(jiff::Span::new().hours(4)).unwrap();
        }
        let last = versions(dir.path());
        assert!(last.len() >= RECENT_KEPT && last.len() <= RECENT_KEPT + DAYS_KEPT as usize);
        assert_eq!(std::fs::read(&last[0].0).unwrap(), b"199");

        // The same again is not another version.
        let n = last.len();
        keep(dir.path(), b"199", now).unwrap();
        assert_eq!(versions(dir.path()).len(), n);
    }
}
