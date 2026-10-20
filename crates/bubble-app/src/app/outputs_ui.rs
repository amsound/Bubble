//! Audio ▸ Outputs: choose which device channels Bubble plays on.
//!
//! Each device's channels are a pool. Stereo pairs, single mono channels and 5.1
//! blocks take channels from it; an option whose channels are taken is greyed out. A
//! 5.1 output takes the first six free channels in a row. Outputs are listed in device
//! and channel order (which gives the letters A, B, …) unless the user reorders them.

use eframe::egui::{self, Color32, RichText, Vec2};

use super::BubbleApp;
use crate::native::{WindowSpec, native_window};
use crate::prefs::OutputPref;

/// Channels taken on `device` by `list` (index = channel).
pub(super) fn used(list: &[OutputPref], device: &str, channels: u16) -> Vec<bool> {
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
pub(super) fn free_run(used: &[bool], len: usize) -> Option<u16> {
    (0..used.len().saturating_sub(len - 1))
        .find(|&s| used[s..s + len].iter().all(|u| !u))
        .map(|s| s as u16)
}

/// Device order (system default first), then channel.
pub(super) fn sort_outputs(list: &mut [OutputPref], devices: &[String]) {
    let pos = |d: &str| devices.iter().position(|x| x == d).unwrap_or(usize::MAX);
    list.sort_by_key(|o| (pos(&o.device), o.first_channel, o.channels));
}

pub(super) struct OutputsDraft {
    pub list: Vec<OutputPref>,
    pub custom_order: bool,
}

const CELL: f32 = 46.0;

impl BubbleApp {
    pub(super) fn open_outputs_window(&mut self) {
        self.devices = bubble_engine::device::list_output_devices().unwrap_or_default();
        if self.dialogs.outputs.is_none() {
            self.dialogs.outputs = Some(OutputsDraft {
                list: self.prefs.effective_outputs(),
                custom_order: self.prefs.outputs_custom_order,
            });
        }
    }

    pub(super) fn outputs_window(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.dialogs.outputs.take() else {
            return;
        };
        let rate = self.prefs.rate();
        // (id, shown name, channels, runs at our rate)
        let default = self.devices.iter().find(|d| d.is_default);
        let mut devices: Vec<(String, String, u16, bool)> = vec![(
            String::new(),
            match default {
                Some(d) => format!("System default ({})", d.name),
                None => "System default".into(),
            },
            default.map_or(2, |d| d.max_channels),
            default.is_none_or(|d| rate != 48_000 || d.supports_48k),
        )];
        devices.extend(self.devices.iter().map(|d| {
            (
                d.name.clone(),
                d.name.clone(),
                d.max_channels,
                rate != 48_000 || d.supports_48k,
            )
        }));
        let order: Vec<String> = devices.iter().map(|d| d.0.clone()).collect();
        let current = self.prefs.effective_outputs();
        let errors = self.output_errors.clone();
        let mut apply = false;
        let mut rescan = false;
        let (open, f) = native_window(
            ctx,
            WindowSpec {
                key: "outputs",
                title: "Outputs",
                size: [880.0, 560.0],
                min_size: [640.0, 380.0],
                on_top: false,
                fit: false,
            },
            |ui| {
                let changed =
                    draft.list != current || draft.custom_order != self.prefs.outputs_custom_order;
                // Apply bar, always at the bottom and impossible to miss when needed.
                egui::Panel::bottom("outputs-apply")
                    .exact_size(52.0)
                    .frame(
                        egui::Frame::new()
                            .fill(if changed {
                                Color32::from_rgb(0x2F, 0x6B, 0xFF).gamma_multiply(0.18)
                            } else {
                                ui.visuals().panel_fill
                            })
                            .inner_margin(egui::Margin::symmetric(12, 10)),
                    )
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            if changed {
                                ui.label(RichText::new("Changes are not applied yet.").strong());
                                ui.label(RichText::new("Applying restarts audio.").weak());
                            } else {
                                ui.label(RichText::new("Outputs are in use as shown.").weak());
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let a = egui::Button::new(
                                        RichText::new("Apply").strong().color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(0x2F, 0x6B, 0xFF))
                                    .min_size(Vec2::new(96.0, 30.0));
                                    if ui
                                        .add_enabled(changed && !draft.list.is_empty(), a)
                                        .clicked()
                                    {
                                        apply = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            changed,
                                            egui::Button::new("Revert")
                                                .min_size(Vec2::new(80.0, 30.0)),
                                        )
                                        .clicked()
                                    {
                                        draft.list = current.clone();
                                        draft.custom_order = self.prefs.outputs_custom_order;
                                    }
                                },
                            );
                        });
                    });
                egui::CentralPanel::default().show(ui, |ui| {
                    for e in &errors {
                        ui.colored_label(Color32::from_rgb(0xF0, 0x9A, 0x2A), e);
                    }
                    ui.columns(2, |cols| {
                        // ---- Devices ----
                        egui::Frame::group(cols[0].style()).show(&mut cols[0], |ui| {
                            ui.set_min_height(ui.available_height());
                            ui.horizontal(|ui| {
                                ui.heading("Devices");
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Rescan").clicked() {
                                            rescan = true;
                                        }
                                    },
                                );
                            });
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .id_salt("out-dev")
                                .show(ui, |ui| {
                                    for (id, name, n, ok) in &devices {
                                        device_grid(
                                            ui, &mut draft, id, name, *n, *ok, rate, &order,
                                        );
                                        ui.add_space(6.0);
                                    }
                                });
                        });
                        // ---- Outputs ----
                        egui::Frame::group(cols[1].style()).show(&mut cols[1], |ui| {
                            ui.set_min_height(ui.available_height());
                            ui.horizontal(|ui| {
                                ui.heading("Outputs");
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if draft.custom_order
                                            && ui
                                                .small_button("Reset order")
                                                .on_hover_text("Back to device and channel order")
                                                .clicked()
                                        {
                                            draft.custom_order = false;
                                            sort_outputs(&mut draft.list, &order);
                                        }
                                    },
                                );
                            });
                            ui.separator();
                            if draft.list.is_empty() {
                                ui.label(RichText::new("Tick channels on the left.").weak());
                            }
                            let n = draft.list.len();
                            let mut swap = None;
                            let mut remove = None;
                            egui::ScrollArea::vertical()
                                .id_salt("out-list")
                                .show(ui, |ui| {
                                    for (i, o) in draft.list.iter_mut().enumerate() {
                                        let w = ui.available_width();
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [24.0, 24.0],
                                                egui::Label::new(
                                                    RichText::new(bubble_project::output_letter(i))
                                                        .strong()
                                                        .size(18.0),
                                                ),
                                            );
                                            ui.add_sized(
                                                [(w - 24.0 - 150.0).max(80.0), 24.0],
                                                egui::TextEdit::singleline(&mut o.alias)
                                                    .hint_text("Alias (optional)"),
                                            );
                                            if ui
                                                .add_enabled(i > 0, egui::Button::new("▲"))
                                                .clicked()
                                            {
                                                swap = Some((i, i - 1));
                                            }
                                            if ui
                                                .add_enabled(i + 1 < n, egui::Button::new("▼"))
                                                .clicked()
                                            {
                                                swap = Some((i, i + 1));
                                            }
                                            if ui.small_button("Remove").clicked() {
                                                remove = Some(i);
                                            }
                                        });
                                        let dev = if o.device.is_empty() {
                                            "System default"
                                        } else {
                                            &o.device
                                        };
                                        ui.horizontal(|ui| {
                                            ui.add_space(30.0);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(format!(
                                                        "{dev} · {}",
                                                        o.channel_text()
                                                    ))
                                                    .small()
                                                    .weak(),
                                                )
                                                .truncate(),
                                            );
                                        });
                                        ui.add_space(6.0);
                                    }
                                });
                            if let Some((a, b)) = swap {
                                draft.list.swap(a, b);
                                draft.custom_order = true;
                            }
                            if let Some(i) = remove {
                                draft.list.remove(i);
                            }
                        });
                    });
                });
            },
        );
        self.child_focus |= f;
        if rescan {
            self.devices = bubble_engine::device::list_output_devices().unwrap_or_default();
        }
        if apply {
            self.prefs.outputs = draft.list.clone();
            self.prefs.outputs_custom_order = draft.custom_order;
            let _ = self.prefs.save();
            self.open_audio();
        }
        if open {
            self.dialogs.outputs = Some(draft);
        }
    }
}

/// One device: channel numbers across the top; stereo, mono and 5.1 rows beneath,
/// each checkbox under the channels it uses.
#[allow(clippy::too_many_arguments)]
fn device_grid(
    ui: &mut egui::Ui,
    draft: &mut OutputsDraft,
    id: &str,
    name: &str,
    n: u16,
    rate_ok: bool,
    rate: u32,
    order: &[String],
) {
    let open = draft.list.iter().any(|o| o.device == id);
    let rate_text = if rate_ok {
        format!("{} kHz", rate as f32 / 1000.0)
    } else {
        format!("no {} kHz", rate as f32 / 1000.0)
    };
    egui::CollapsingHeader::new(RichText::new(format!("{name}   {n} ch · {rate_text}")).strong())
        .id_salt(("dev", id))
        .default_open(open)
        .show(ui, |ui| {
            if !rate_ok {
                ui.colored_label(
                    Color32::from_rgb(0xF0, 0x9A, 0x2A),
                    format!("This device does not offer {} kHz.", rate as f32 / 1000.0),
                );
            }
            let taken = used(&draft.list, id, n);
            let mut toggle: Option<OutputPref> = None;
            let find = |list: &[OutputPref], first: u16, ch: u16| {
                list.iter()
                    .position(|o| o.device == id && o.first_channel == first && o.channels == ch)
            };
            egui::ScrollArea::horizontal()
                .id_salt(("devgrid", id))
                .show(ui, |ui| {
                    egui::Grid::new(("grid", id))
                        .num_columns(n as usize + 1)
                        .min_col_width(CELL)
                        .max_col_width(CELL)
                        .spacing([2.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("");
                            for c in 0..n {
                                ui.label(RichText::new(format!("{}", c + 1)).small().weak());
                            }
                            ui.end_row();

                            let mut row =
                                |ui: &mut egui::Ui, label: &str, width: u16, starts: Vec<u16>| {
                                    ui.label(RichText::new(label).weak());
                                    let mut c = 0u16;
                                    while c < n {
                                        if starts.contains(&c) {
                                            let pos = find(&draft.list, c, width);
                                            let free = (c..c + width).all(|x| !taken[x as usize]);
                                            let mut on = pos.is_some();
                                            let text = if width == 1 {
                                                format!("{}", c + 1)
                                            } else {
                                                format!("{}–{}", c + 1, c + width)
                                            };
                                            let r = ui.add_enabled(
                                                on || free,
                                                egui::Checkbox::new(&mut on, text),
                                            );
                                            if r.changed() {
                                                toggle = Some(OutputPref {
                                                    device: id.to_string(),
                                                    first_channel: c,
                                                    channels: width,
                                                    alias: String::new(),
                                                });
                                            }
                                            // A wide option spans its channels' columns.
                                            for _ in 1..width.min(n - c) {
                                                ui.label("");
                                            }
                                            c += width;
                                        } else {
                                            ui.label("");
                                            c += 1;
                                        }
                                    }
                                    ui.end_row();
                                };
                            row(ui, "Stereo", 2, (0..n / 2).map(|p| p * 2).collect());
                            row(ui, "Mono", 1, (0..n).collect());
                            if n >= 6 {
                                // Existing 5.1 outputs, plus the next free block of six.
                                let mut starts: Vec<u16> = draft
                                    .list
                                    .iter()
                                    .filter(|o| o.device == id && o.channels == 6)
                                    .map(|o| o.first_channel)
                                    .collect();
                                if let Some(s) = free_run(&taken, 6)
                                    && starts.iter().all(|x| s + 6 <= *x || *x + 6 <= s)
                                {
                                    starts.push(s);
                                }
                                starts.sort();
                                if starts.is_empty() {
                                    ui.label(RichText::new("5.1").weak());
                                    ui.label(RichText::new("needs 6 free channels").small().weak());
                                    ui.end_row();
                                } else {
                                    row(ui, "5.1", 6, starts);
                                }
                            }
                        });
                });
            ui.add_space(14.0);
            if let Some(o) = toggle {
                match find(&draft.list, o.first_channel, o.channels) {
                    Some(i) => {
                        draft.list.remove(i);
                    }
                    None if draft.list.len() < bubble_project::MAX_OUTPUTS => {
                        draft.list.push(o);
                        if !draft.custom_order {
                            sort_outputs(&mut draft.list, order);
                        }
                    }
                    None => {}
                }
            }
        });
}

/// Audio ▸ Reassign Cart Outputs: move carts from one output to another.
pub(super) struct ReassignDraft {
    /// `None` = carts on any output.
    pub from: Option<usize>,
    pub to: usize,
    /// 0 all pages, 1 this page, 2 a group, 3 a range of buttons on this page.
    pub scope: u8,
    pub group: Option<uuid::Uuid>,
    pub first: u32,
    pub last: u32,
}

impl BubbleApp {
    pub(super) fn reassign_window(
        &mut self,
        ctx: &egui::Context,
        snap: &super::Snapshot,
        intents: &mut Vec<super::Intent>,
    ) {
        let Some(mut d) = self.dialogs.reassign.take() else {
            return;
        };
        let n = self.prefs.effective_outputs().len().max(1);
        let labels: Vec<String> = (0..n).map(|i| self.output_label(i, false)).collect();
        let page_cells = snap
            .project
            .pages
            .get(snap.page)
            .map_or(1, |p| p.rows as u32 * p.cols as u32);
        let mut go = false;
        let mut close = false;
        let (open, f) = native_window(
            ctx,
            WindowSpec {
                key: "reassign",
                title: "Reassign Cart Outputs",
                size: [500.0, 300.0],
                min_size: [460.0, 200.0],
                on_top: false,
                fit: true,
            },
            |ui| {
                ui.add_space(4.0);
                egui::Grid::new("reassign")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Carts on").weak());
                        egui::ComboBox::from_id_salt("re-from")
                            .width(220.0)
                            .selected_text(
                                d.from.map_or("Any output".to_string(), |i| {
                                    labels[i.min(n - 1)].clone()
                                }),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut d.from, None, "Any output");
                                for (i, l) in labels.iter().enumerate() {
                                    ui.selectable_value(&mut d.from, Some(i), l);
                                }
                            });
                        ui.end_row();
                        ui.label(RichText::new("Move to").weak());
                        egui::ComboBox::from_id_salt("re-to")
                            .width(220.0)
                            .selected_text(labels[d.to.min(n - 1)].clone())
                            .show_ui(ui, |ui| {
                                for (i, l) in labels.iter().enumerate() {
                                    ui.selectable_value(&mut d.to, i, l);
                                }
                            });
                        ui.end_row();
                        ui.label(RichText::new("Apply to").weak());
                        ui.vertical(|ui| {
                            ui.radio_value(&mut d.scope, 0, "All pages");
                            ui.radio_value(&mut d.scope, 1, "This page only");
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut d.scope, 2, "Group");
                                ui.add_enabled_ui(d.scope == 2, |ui| {
                                    super::group_combo(ui, "re-group", &snap.project, &mut d.group);
                                });
                            });
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut d.scope, 3, "Buttons");
                                ui.add_enabled_ui(d.scope == 3, |ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut d.first).range(1..=page_cells),
                                    );
                                    ui.label("to");
                                    ui.add(egui::DragValue::new(&mut d.last).range(1..=page_cells));
                                    ui.label(RichText::new("on this page").weak());
                                });
                            });
                        });
                        ui.end_row();
                    });
                crate::native::dialog_bar(ui, |ui| {
                    let ready = d.scope != 2 || d.group.is_some();
                    if ui
                        .add_enabled(ready, crate::native::primary("Reassign"))
                        .clicked()
                    {
                        go = true;
                    }
                    if ui.add(crate::native::secondary("Close")).clicked() {
                        close = true;
                    }
                });
            },
        );
        self.child_focus |= f;
        if go {
            let scope = match d.scope {
                0 => bubble_control::ReassignScope::All,
                1 => bubble_control::ReassignScope::Page(snap.page),
                2 => bubble_control::ReassignScope::Group(d.group.unwrap_or_default()),
                _ => bubble_control::ReassignScope::Buttons {
                    page: snap.page,
                    first: d.first.min(d.last),
                    last: d.first.max(d.last),
                },
            };
            intents.push(super::Intent::ReassignOutputs(
                d.from.map(bubble_project::output_letter),
                bubble_project::output_letter(d.to),
                scope,
            ));
        }
        if open && !close {
            self.dialogs.reassign = Some(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(first: u16, ch: u16) -> OutputPref {
        OutputPref {
            device: "D".into(),
            first_channel: first,
            channels: ch,
            alias: String::new(),
        }
    }

    #[test]
    fn surround_takes_the_first_free_six_after_a_stereo_pair() {
        let list = vec![out(0, 2)];
        let u = used(&list, "D", 8);
        assert_eq!(free_run(&u, 6), Some(2), "channels 3–8");
        let list = vec![out(0, 2), out(2, 2)];
        assert_eq!(free_run(&used(&list, "D", 8), 6), None, "only 4 left");
        assert_eq!(free_run(&used(&[], "D", 8), 6), Some(0));
    }

    #[test]
    fn outputs_sort_by_device_then_channel() {
        let mut l = vec![out(4, 2), out(0, 2), out(2, 2)];
        l.push(OutputPref {
            device: String::new(),
            ..out(0, 2)
        });
        sort_outputs(&mut l, &[String::new(), "D".into()]);
        let k: Vec<(&str, u16)> = l
            .iter()
            .map(|o| (o.device.as_str(), o.first_channel))
            .collect();
        assert_eq!(k, [("", 0), ("D", 0), ("D", 2), ("D", 4)]);
    }
}
