//! MIX RACK — one vertical channel strip per track, plus the master bus.
//!
//! Every channel has a fader, mute/solo, an effects chain and a fixed list of
//! insert slots. The insert slots are the hook for plugin hosting: a slot holds
//! a scanned CLAP / VST3 / VST unit (name, path, wet, enabled), so the DSP
//! runtime of the next stage reads exactly this list instead of the UI having
//! to change. The master strip owns gain, inserts, effects and a limiter
//! section; only the gain is wired to the engine today.

use eframe::egui::{self, RichText};

use crate::library::PluginEntry;
use crate::theme::Theme;
use crate::tracks::{Effect, EffectKind, Master, PluginInst, Track, INSERT_SLOTS};

const STRIP_W: f32 = 152.0;
/// Fader travel of a track (0 .. 1.5 gain).
const FADER_MAX: f32 = 1.5;
/// Fader travel of the master (0 .. 1.2 gain).
const MASTER_FADER_MAX: f32 = 1.2;

/// What the rack wants the app to do after a frame.
#[derive(Default)]
pub struct RackDirty {
    pub changed: bool,
    pub select: Option<usize>,
    pub gain: Option<f32>,
    pub open_fx: Option<usize>,
}

pub fn show(
    ui: &mut egui::Ui,
    theme: &Theme,
    tracks: &mut [Track],
    selected: usize,
    master: &mut Master,
    gain: f32,
    meter: f32,
    available: &[PluginEntry],
) -> RackDirty {
    let mut out = RackDirty::default();

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("MIX RACK")
                .size(10.0)
                .strong()
                .color(theme.accent),
        );
        ui.label(
            RichText::new(format!(
                "master + {} track(s) · {} insert slot(s) per channel",
                tracks.len(),
                INSERT_SLOTS
            ))
            .size(9.0)
            .color(theme.dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("inserts hold scanned plugins · DSP hosting = next stage")
                    .size(9.0)
                    .color(theme.dim),
            );
        });
    });
    ui.separator();

    let height = (ui.available_height() - 4.0).max(140.0);
    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(STRIP_W, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        master_strip(ui, theme, master, gain, meter, available, &mut out, height);
                    },
                );
                ui.add_space(2.0);
                ui.separator();
                for i in 0..tracks.len() {
                    ui.allocate_ui_with_layout(
                        egui::vec2(STRIP_W, height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            track_strip(
                                ui,
                                theme,
                                i,
                                &mut tracks[i],
                                i == selected,
                                available,
                                &mut out,
                                height,
                            );
                        },
                    );
                }
            });
        });

    out
}

fn track_strip(
    ui: &mut egui::Ui,
    theme: &Theme,
    i: usize,
    track: &mut Track,
    selected: bool,
    available: &[PluginEntry],
    out: &mut RackDirty,
    height: f32,
) {
    let color = track.color;
    let fader_h = (height - 250.0).clamp(56.0, 240.0);

    ui.push_id(("strip", i), |ui| {
        egui::Frame::none()
            .fill(if selected { theme.panel_hi } else { theme.panel })
            .stroke(egui::Stroke::new(
                1.0_f32,
                if selected { color } else { theme.grid },
            ))
            .rounding(egui::Rounding::same(3.0))
            .inner_margin(egui::Margin::symmetric(6.0, 5.0))
            .show(ui, |ui| {
                ui.set_width(STRIP_W - 16.0);

                ui.horizontal(|ui| {
                    let name = ui.add(
                        egui::Label::new(RichText::new(&track.name).size(10.0).strong().color(color))
                            .sense(egui::Sense::click()),
                    );
                    if name.clicked() {
                        out.select = Some(i);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("fx")
                            .on_hover_text("open this track's FX page")
                            .clicked()
                        {
                            out.open_fx = Some(i);
                        }
                    });
                });
                let (bar, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 2.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, egui::Rounding::ZERO, color);

                ui.vertical_centered(|ui| {
                    if fader(ui, &mut track.volume, FADER_MAX, fader_h, theme, color) {
                        out.changed = true;
                    }
                });
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(db_label(track.volume))
                            .size(9.0)
                            .monospace()
                            .color(theme.cream),
                    );
                });

                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(track.muted, RichText::new("M").size(9.0))
                        .on_hover_text("mute")
                        .clicked()
                    {
                        track.muted = !track.muted;
                        out.changed = true;
                    }
                    if ui
                        .selectable_label(track.solo, RichText::new("S").size(9.0))
                        .on_hover_text("solo")
                        .clicked()
                    {
                        track.solo = !track.solo;
                        out.changed = true;
                    }
                    ui.label(
                        RichText::new(format!("{}/{} ins", track.plugins.len(), INSERT_SLOTS))
                            .size(8.0)
                            .color(theme.dim),
                    );
                });

                ui.label(RichText::new("INSERTS").size(8.0).color(theme.dim));
                if insert_slots(ui, theme, &mut track.plugins, available) {
                    out.changed = true;
                }

                ui.label(RichText::new("EFFECTS").size(8.0).color(theme.dim));
                ui.horizontal(|ui| {
                    ui.menu_button(RichText::new("+ fx").size(9.0).color(theme.accent), |ui| {
                        for kind in EffectKind::ALL {
                            if ui.button(kind.label()).clicked() {
                                track.effects.push(Effect::new(kind));
                                out.changed = true;
                                ui.close_menu();
                            }
                        }
                    });
                });
                if effects(ui, theme, &mut track.effects) {
                    out.changed = true;
                }
            });
    });
}

fn master_strip(
    ui: &mut egui::Ui,
    theme: &Theme,
    master: &mut Master,
    gain: f32,
    meter: f32,
    available: &[PluginEntry],
    out: &mut RackDirty,
    height: f32,
) {
    let accent = theme.cream;
    let fader_h = (height - 270.0).clamp(56.0, 240.0);

    ui.push_id("master-strip", |ui| {
        egui::Frame::none()
            .fill(theme.panel)
            .stroke(egui::Stroke::new(1.0_f32, theme.accent_hot))
            .rounding(egui::Rounding::same(3.0))
            .inner_margin(egui::Margin::symmetric(6.0, 5.0))
            .show(ui, |ui| {
                ui.set_width(STRIP_W - 16.0);

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("MASTER")
                            .size(10.0)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("OUT")
                                .size(8.0)
                                .color(theme.dim),
                        );
                    });
                });
                let (bar, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 2.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(bar, egui::Rounding::ZERO, theme.accent_hot);

                ui.vertical_centered(|ui| {
                    let mut g = gain;
                    if fader(ui, &mut g, MASTER_FADER_MAX, fader_h, theme, theme.accent_hot) {
                        out.gain = Some(g);
                        out.changed = true;
                    }
                });
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(db_label(gain))
                            .size(9.0)
                            .monospace()
                            .color(theme.cream),
                    );
                });

                // Output meter.
                let (mrect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(mrect, egui::Rounding::same(2.0), theme.panel_hi);
                let w = mrect.width() * meter.clamp(0.0, 1.0);
                if w > 0.5 {
                    let fill = egui::Rect::from_min_size(mrect.min, egui::vec2(w, mrect.height()));
                    let col = if meter > 0.9 { theme.red } else { theme.accent_hot };
                    painter.rect_filled(fill, egui::Rounding::same(2.0), col);
                }

                ui.label(RichText::new("INSERTS").size(8.0).color(theme.dim));
                if insert_slots(ui, theme, &mut master.plugins, available) {
                    out.changed = true;
                }

                ui.label(RichText::new("EFFECTS").size(8.0).color(theme.dim));
                ui.horizontal(|ui| {
                    ui.menu_button(RichText::new("+ fx").size(9.0).color(theme.accent), |ui| {
                        for kind in EffectKind::ALL {
                            if ui.button(kind.label()).clicked() {
                                master.effects.push(Effect::new(kind));
                                out.changed = true;
                                ui.close_menu();
                            }
                        }
                    });
                });
                if effects(ui, theme, &mut master.effects) {
                    out.changed = true;
                }

                ui.label(RichText::new("LIMITER").size(8.0).color(theme.dim));
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut master.limiter, "")
                        .on_hover_text("brickwall limiter on the master bus (DSP: next stage)")
                        .changed()
                    {
                        out.changed = true;
                    }
                    ui.label(
                        RichText::new("on")
                            .size(9.0)
                            .color(if master.limiter { accent } else { theme.dim }),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ceil").size(8.0).color(theme.dim));
                    ui.spacing_mut().slider_width = 62.0;
                    ui.add(egui::Slider::new(&mut master.ceiling_db, -6.0..=0.0).show_value(false))
                    .on_hover_text("ceiling, dBFS — limiter DSP: next stage");
                    ui.label(
                        RichText::new(format!("{:.1}", master.ceiling_db))
                            .size(8.0)
                            .monospace()
                            .color(theme.dim),
                    );
                });
            });
    });
}

/// Fixed list of insert slots. An empty slot opens the plugin picker, a filled
/// one holds the scanned unit for the future DSP runtime.
fn insert_slots(
    ui: &mut egui::Ui,
    theme: &Theme,
    plugins: &mut Vec<PluginInst>,
    available: &[PluginEntry],
) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;

    for slot in 0..INSERT_SLOTS {
        if slot < plugins.len() {
            let pl = &mut plugins[slot];
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}", slot + 1))
                        .size(8.0)
                        .monospace()
                        .color(theme.dim),
                );
                if ui.checkbox(&mut pl.enabled, "").changed() {
                    changed = true;
                }
                let name: String = if pl.name.chars().count() > 10 {
                    pl.name.chars().take(9).collect::<String>() + "…"
                } else {
                    pl.name.clone()
                };
                let color = if pl.enabled { theme.cream } else { theme.dim };
                ui.label(RichText::new(name).size(9.0).color(color))
                    .on_hover_text(format!(
                        "{} ({})\n{}\nwet {:.0}% · hosting: next stage",
                        pl.name,
                        pl.kind.label(),
                        pl.path.display(),
                        pl.wet * 100.0
                    ));
                if ui.small_button("x").clicked() {
                    remove = Some(slot);
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}", slot + 1))
                        .size(8.0)
                        .monospace()
                        .color(theme.dim),
                );
                ui.menu_button(RichText::new("+ insert").size(9.0).color(theme.dim), |ui| {
                    ui.label(
                        RichText::new("insert slot — hosting lands in the next stage")
                            .size(9.0)
                            .color(theme.dim),
                    );
                    if available.is_empty() {
                        ui.label(
                            RichText::new("no CLAP / VST3 / VST scanned yet")
                                .size(10.0)
                                .color(theme.red),
                        );
                    }
                    for entry in available.iter().take(48) {
                        if ui
                            .button(format!("{}  ·  {}", entry.name, entry.kind.label()))
                            .clicked()
                        {
                            plugins.push(PluginInst {
                                name: entry.name.clone(),
                                path: entry.path.clone(),
                                kind: entry.kind,
                                wet: 1.0,
                                enabled: true,
                            });
                            changed = true;
                            ui.close_menu();
                        }
                    }
                });
            });
        }
    }

    if let Some(i) = remove {
        plugins.remove(i);
        changed = true;
    }
    changed
}

fn effects(ui: &mut egui::Ui, theme: &Theme, effects: &mut Vec<Effect>) -> bool {
    let mut changed = false;
    let mut remove: Option<usize> = None;

    for (i, fx) in effects.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if ui.checkbox(&mut fx.enabled, "").changed() {
                changed = true;
            }
            ui.label(RichText::new(fx.kind.label()).size(9.0).color(theme.accent));
            ui.spacing_mut().slider_width = 56.0;
            if ui
                .add(egui::Slider::new(&mut fx.amount, 0.0..=1.0).show_value(false))
                .changed()
            {
                changed = true;
            }
            if ui.small_button("x").clicked() {
                remove = Some(i);
            }
        });
    }

    if let Some(i) = remove {
        effects.remove(i);
        changed = true;
    }
    changed
}

/// Vertical fader: click or drag anywhere on the track to set the value.
fn fader(
    ui: &mut egui::Ui,
    value: &mut f32,
    max: f32,
    height: f32,
    theme: &Theme,
    color: egui::Color32,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(34.0, height), egui::Sense::click_and_drag());
    let mut changed = false;

    if resp.clicked() || resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let t = ((rect.bottom() - pos.y) / rect.height().max(1.0)).clamp(0.0, 1.0);
            *value = t * max;
            changed = true;
        }
    }

    let track = egui::Rect::from_min_max(
        egui::pos2(rect.center().x - 3.0, rect.top()),
        egui::pos2(rect.center().x + 3.0, rect.bottom()),
    );
    let painter = ui.painter();
    painter.rect_filled(track, egui::Rounding::same(2.0), theme.panel_hi);

    let t = (*value / max).clamp(0.0, 1.0);
    let y = rect.bottom() - t * rect.height();
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(track.left(), y), egui::pos2(track.right(), rect.bottom())),
        egui::Rounding::same(2.0),
        color.gamma_multiply(0.8),
    );
    let cap = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 2.0, y - 3.0),
        egui::pos2(rect.right() - 2.0, y + 3.0),
    );
    painter.rect_filled(cap, egui::Rounding::same(2.0), theme.cream);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }

    changed
}

fn db_label(gain: f32) -> String {
    if gain <= 0.0005 {
        "-inf dB".to_string()
    } else {
        format!("{:+.1} dB", 20.0 * gain.log10())
    }
}
