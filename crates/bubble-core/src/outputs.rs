//! Which device channels Bubble plays on (Audio ▸ Outputs, or Settings ▸ Audio).
//!
//! Each device's channels are a pool. Stereo pairs, single mono channels and 5.1
//! blocks take channels from it. Outputs are lettered by their place in the list
//! (A, B, …); carts name the letter, and each machine says what the letter is.

use std::collections::HashMap;

use crate::app::Core;
use crate::prefs::OutputPref;

/// Channels taken on `device` by `list` (index = channel).
pub fn used(list: &[OutputPref], device: &str, channels: u16) -> Vec<bool> {
    let mut u = vec![false; channels as usize];
    for o in list.iter().filter(|o| o.device == device) {
        for c in o.first_channel..o.first_channel + o.channels {
            if let Some(x) = u.get_mut(c as usize) {
                *x = true;
            }
        }
    }
    u
}

/// First channel of `len` consecutive free channels.
pub fn free_run(used: &[bool], len: usize) -> Option<u16> {
    (0..used.len().saturating_sub(len - 1))
        .find(|&s| used[s..s + len].iter().all(|u| !u))
        .map(|s| s as u16)
}

/// Device order (system default first), then channel.
pub fn sort_outputs(list: &mut [OutputPref], devices: &[String]) {
    let pos = |d: &str| devices.iter().position(|x| x == d).unwrap_or(usize::MAX);
    list.sort_by_key(|o| (pos(&o.device), o.first_channel, o.channels));
}

/// The channel choices for output `index` of `list` on a device with `channels`
/// channels: (first channel, width), stereo pairs first, then mono, then 5.1, each
/// with whether it is free (not taken by another output).
pub fn channel_choices(
    list: &[OutputPref],
    index: usize,
    device: &str,
    channels: u16,
) -> Vec<(u16, u16, bool)> {
    let others: Vec<OutputPref> = list
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, o)| o.clone())
        .collect();
    let taken = used(&others, device, channels);
    let free = |first: u16, width: u16| (first..first + width).all(|c| !taken[c as usize]);
    let mut out = Vec::new();
    for p in 0..channels / 2 {
        out.push((p * 2, 2, free(p * 2, 2)));
    }
    for c in 0..channels {
        out.push((c, 1, free(c, 1)));
    }
    if channels >= 6 {
        for s in 0..=channels - 6 {
            if s % 2 == 0 {
                out.push((s, 6, free(s, 6)));
            }
        }
    }
    out
}

/// "Ch 1–2", as [`OutputPref::channel_text`] says it.
pub fn channel_text(first: u16, width: u16) -> String {
    OutputPref {
        device: String::new(),
        first_channel: first,
        channels: width,
        alias: String::new(),
    }
    .channel_text()
}

/// A new output: the first free stereo pair (or mono channel) on the first device
/// that has one, the system default first. `devices` are (name, channels), "" being
/// the system default.
pub fn next_free(list: &[OutputPref], devices: &[(String, u16)]) -> Option<OutputPref> {
    for (d, n) in devices {
        let taken = used(list, d, *n);
        for width in [2usize, 1] {
            if let Some(first) = free_run(&taken, width) {
                return Some(OutputPref {
                    device: d.clone(),
                    first_channel: first,
                    channels: width as u16,
                    alias: String::new(),
                });
            }
        }
    }
    None
}

/// Old letter → new letter for every output whose letter changes between `old` and
/// `new` (matched by device and channels); removed outputs map to "A".
pub fn letter_changes(old: &[OutputPref], new: &[OutputPref]) -> HashMap<String, String> {
    let same = |a: &OutputPref, b: &OutputPref| {
        a.device == b.device && a.first_channel == b.first_channel && a.channels == b.channels
    };
    old.iter()
        .enumerate()
        .filter_map(|(i, o)| {
            let from = bubble_project::output_letter(i);
            let to = new.iter().position(|n| same(o, n)).map_or_else(
                || bubble_project::output_letter(0),
                bubble_project::output_letter,
            );
            (from != to).then(|| (from.to_string(), to.to_string()))
        })
        .collect()
}

impl Core {
    /// Makes `list` this machine's outputs and restarts audio. Letters are places in
    /// the list, so removing or reordering outputs changes them: carts follow their
    /// output to its new letter, and carts on a removed output go to Output A (one
    /// undo step).
    pub fn set_outputs(&mut self, list: Vec<OutputPref>, custom_order: bool) {
        let map = letter_changes(&self.prefs.effective_outputs(), &list);
        if !map.is_empty() {
            let n = self.lock().remap_outputs(&map);
            crate::log::info(&format!("outputs changed: {map:?}; {n} cart(s) updated"));
        }
        self.prefs.outputs = list;
        self.prefs.outputs_custom_order = custom_order;
        self.save_prefs();
        self.open_audio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(device: &str, first: u16, ch: u16) -> OutputPref {
        OutputPref {
            device: device.into(),
            first_channel: first,
            channels: ch,
            alias: String::new(),
        }
    }

    #[test]
    fn choices_mark_what_other_outputs_take() {
        let list = vec![out("D", 0, 2), out("D", 2, 2)];
        let c = channel_choices(&list, 1, "D", 6);
        // Stereo 1–2 is the other output's; 3–4 is this one's own, so free.
        assert_eq!(&c[..3], &[(0, 2, false), (2, 2, true), (4, 2, true)]);
        assert!(c.contains(&(0, 1, false)) && c.contains(&(2, 1, true)));
        assert_eq!(c.iter().filter(|x| x.1 == 6).count(), 1);
    }

    #[test]
    fn a_new_output_takes_the_next_free_pair() {
        let list = vec![out("", 0, 2)];
        let devs = vec![(String::new(), 2), ("D".to_string(), 4)];
        assert_eq!(next_free(&list, &devs), Some(out("D", 0, 2)));
        let full = vec![out("", 0, 2), out("D", 0, 2), out("D", 2, 1)];
        assert_eq!(next_free(&full, &devs), Some(out("D", 3, 1)));
    }

    #[test]
    fn carts_follow_outputs_and_removed_ones_go_to_a() {
        let old = vec![out("", 0, 2), out("D", 0, 2), out("D", 2, 2)];
        let new = vec![out("D", 2, 2), out("", 0, 2)];
        let m = letter_changes(&old, &new);
        assert_eq!(m["A"], "B");
        assert_eq!(m["B"], "A");
        assert_eq!(m["C"], "A");
    }
}
