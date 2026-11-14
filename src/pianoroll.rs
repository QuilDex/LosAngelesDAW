//! MIDI piano roll: a simple seconds-based note grid with drag-to-move notes.
//!
//! Kept deliberately close to the DAW's grid-free philosophy: the "grid" is a
//! snap step the user can change (or bypass with Alt), not a musical meter.

use eframe::egui;

use crate::theme::Theme;

pub const KEY_W: f32 = 46.0;
pub const ROW_H: f32 = 12.0;
pub const PITCH_HI: u8 = 96;
pub const PITCH_LO: u8 = 36;

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn note_name(pitch: u8) -> String {
    let n = NAMES[(pitch % 12) as usize];
    format!("{n}{}", pitch as i32 / 12 - 1)
}

fn is_black(pitch: i32) -> bool {
    matches!(pitch.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

#[derive(Clone, Copy)]
pub struct Note {
    pub start: f32,
    pub len: f32,
    pub pitch: u8,
    pub vel: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Move,
    Resize,
}

#[derive(Clone, Copy)]
struct Drag {
    idx: usize,
    mode: Mode,
    grab_start: f32,
    grab_len: f32,
    grab_pitch: i32,
    pointer_t0: f32,
    pointer_p0: i32,
}

#[derive(Clone)]
pub struct PianoRoll {
    pub notes: Vec<Note>,
    /// Snap step in seconds (0 disables snapping).
    pub grid: f32,
    pub zoom: f32,
    pub scroll: f32,
    drag: Option<Drag>,
    pub changed: bool,
}

impl Default for PianoRoll {
    fn default() -> Self {
        Self {
            notes: Vec::new(),
            grid: 0.125,
            zoom: 220.0,
            scroll: 0.0,
            drag: None,
            changed: true,
        }
    }
}

impl PianoRoll {
    pub fn show(&mut self, ui: &mut egui::Ui, theme: &Theme, playhead: f32) {
        let (response, painter) =
            ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, egui::Rounding::ZERO, theme.bg);
        if rect.width() < KEY_W + 40.0 || rect.height() < ROW_H * 3.0 {
            return;
        }

        let grid_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + KEY_W, rect.top()),
            rect.max,
        );
        let pitch_y = |p: i32| rect.top() + (PITCH_HI as i32 - p) as f32 * ROW_H;
        let pitch_at = |y: f32| PITCH_HI as i32 - ((y - rect.top()) / ROW_H).floor() as i32;
        let x_of = |t: f32| grid_rect.left() + (t - self.scroll) * self.zoom;

        // -- zoom / scroll ------------------------------------------------
        if let Some(hp) = response.hover_pos() {
            if grid_rect.contains(hp) {
                let (dy, dx, ctrl) = ui.input(|i| {
                    (
                        i.smooth_scroll_delta.y,
                        i.smooth_scroll_delta.x,
                        i.modifiers.ctrl || i.modifiers.command,
                    )
                });
                if ctrl && dy.abs() > 0.1 {
                    let before = self.scroll + (hp.x - grid_rect.left()) / self.zoom;
                    self.zoom = (self.zoom * (dy * 0.003).exp()).clamp(20.0, 3000.0);
                    self.scroll = before - (hp.x - grid_rect.left()) / self.zoom;
                } else if dy.abs() > 0.1 || dx.abs() > 0.1 {
                    self.scroll -= (dy + dx) / self.zoom;
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.scroll -= response.drag_delta().x / self.zoom;
        }
        self.scroll = self.scroll.max(0.0);

        let snap_on = !ui.input(|i| i.modifiers.alt);
        let snap = |t: f32| -> f32 {
            if snap_on && self.grid > 0.0 {
                (t / self.grid).round() * self.grid
            } else {
                t
            }
        };

        let t_at = |x: f32| self.scroll + (x - grid_rect.left()) / self.zoom;
        let hit = |t: f32, p: i32| -> Option<(usize, Mode)> {
            for (i, n) in self.notes.iter().enumerate().rev() {
                if n.pitch as i32 != p {
                    continue;
                }
                if t >= n.start && t <= n.start + n.len {
                    let edge = (n.len * 0.25).min(0.12);
                    let mode = if t >= n.start + n.len - edge {
                        Mode::Resize
                    } else {
                        Mode::Move
                    };
                    return Some((i, mode));
                }
            }
            None
        };

        let pointer = response.interact_pointer_pos();

        // -- create / delete ---------------------------------------------
        let secondary = ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
        if response.clicked() {
            if let Some(pos) = pointer {
                if grid_rect.contains(pos) {
                    let t = t_at(pos.x);
                    let p = pitch_at(pos.y).clamp(PITCH_LO as i32, PITCH_HI as i32);
                    if hit(t, p).is_none() {
                        let step = if self.grid > 0.0 { self.grid } else { 0.25 };
                        self.notes.push(Note {
                            start: snap(t).max(0.0),
                            len: step,
                            pitch: p as u8,
                            vel: 0.8,
                        });
                        self.changed = true;
                    }
                }
            }
        }
        if secondary {
            if let Some(pos) = pointer {
                if grid_rect.contains(pos) {
                    let t = t_at(pos.x);
                    let p = pitch_at(pos.y);
                    if let Some((idx, _)) = hit(t, p) {
                        self.notes.remove(idx);
                        self.drag = None;
                        self.changed = true;
                    }
                }
            }
        }

        // -- drag notes ---------------------------------------------------
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = pointer {
                if grid_rect.contains(pos) {
                    let t = t_at(pos.x);
                    let p = pitch_at(pos.y);
                    if let Some((idx, mode)) = hit(t, p) {
                        let n = self.notes[idx];
                        self.drag = Some(Drag {
                            idx,
                            mode,
                            grab_start: n.start,
                            grab_len: n.len,
                            grab_pitch: n.pitch as i32,
                            pointer_t0: t,
                            pointer_p0: p,
                        });
                    }
                }
            }
        }
        if let Some(d) = self.drag {
            if response.dragged() {
                if let Some(pos) = pointer {
                    let t = t_at(pos.x);
                    let p = pitch_at(pos.y);
                    if let Some(n) = self.notes.get_mut(d.idx) {
                        match d.mode {
                            Mode::Move => {
                                n.start = snap(d.grab_start + (t - d.pointer_t0)).max(0.0);
                                let np = d.grab_pitch + (p - d.pointer_p0);
                                n.pitch = np.clamp(PITCH_LO as i32, PITCH_HI as i32) as u8;
                            }
                            Mode::Resize => {
                                let min_len = if self.grid > 0.0 {
                                    self.grid * 0.5
                                } else {
                                    0.02
                                };
                                n.len = snap(d.grab_len + (t - d.pointer_t0)).max(min_len);
                            }
                        }
                    }
                    self.changed = true;
                }
            } else {
                self.drag = None;
            }
        }

        // -- keyboard -----------------------------------------------------
        let kb = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + KEY_W, rect.bottom()));
        painter.rect_filled(kb, egui::Rounding::ZERO, theme.panel);
        for p in PITCH_LO..=PITCH_HI {
            let p = p as i32;
            let y = pitch_y(p);
            if y > rect.bottom() {
                break;
            }
            let row = egui::Rect::from_min_max(
                egui::pos2(kb.left(), y),
                egui::pos2(kb.right(), (y + ROW_H).min(rect.bottom())),
            );
            let black = is_black(p);
            painter.rect_filled(
                row,
                egui::Rounding::ZERO,
                if black { theme.bg } else { theme.panel_hi },
            );
            painter.line_segment(
                [row.left_bottom(), row.right_bottom()],
                egui::Stroke::new(1.0_f32, theme.grid.gamma_multiply(0.6)),
            );
            if p % 12 == 0 {
                painter.text(
                    egui::pos2(kb.right() - 4.0, y + ROW_H * 0.5),
                    egui::Align2::RIGHT_CENTER,
                    note_name(p as u8),
                    egui::FontId::proportional(9.0),
                    theme.dim,
                );
            }
        }

        // -- rows on the grid ---------------------------------------------
        let gp = painter.with_clip_rect(grid_rect);
        for p in PITCH_LO..=PITCH_HI {
            let p = p as i32;
            let y = pitch_y(p);
            if y > rect.bottom() {
                break;
            }
            if is_black(p) {
                gp.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(grid_rect.left(), y),
                        egui::pos2(grid_rect.right(), y + ROW_H),
                    ),
                    egui::Rounding::ZERO,
                    theme.bg.gamma_multiply(1.06),
                );
            }
            gp.line_segment(
                [
                    egui::pos2(grid_rect.left(), y + ROW_H),
                    egui::pos2(grid_rect.right(), y + ROW_H),
                ],
                egui::Stroke::new(1.0_f32, theme.grid.gamma_multiply(0.55)),
            );
        }

        // -- time grid -----------------------------------------------------
        let step = if self.grid > 0.0 {
            let mut s = self.grid;
            while s * self.zoom < 14.0 {
                s *= 2.0;
            }
            s
        } else {
            let mut s = 0.25;
            while s * self.zoom < 40.0 {
                s *= 2.0;
            }
            s
        };
        let mut t = (self.scroll / step).floor() * step;
        let t_end = self.scroll + grid_rect.width() / self.zoom;
        let mut i = 0;
        while t <= t_end + step {
            if t >= 0.0 {
                let x = x_of(t);
                let strong = i % 4 == 0;
                gp.line_segment(
                    [
                        egui::pos2(x, grid_rect.top()),
                        egui::pos2(x, grid_rect.bottom()),
                    ],
                    egui::Stroke::new(
                        1.0_f32,
                        if strong {
                            theme.grid
                        } else {
                            theme.grid.gamma_multiply(0.45)
                        },
                    ),
                );
            }
            t += step;
            i += 1;
        }

        // -- notes ---------------------------------------------------------
        for (idx, n) in self.notes.iter().enumerate() {
            let x0 = x_of(n.start);
            let x1 = x_of(n.start + n.len);
            if x1 < grid_rect.left() || x0 > grid_rect.right() {
                continue;
            }
            let y = pitch_y(n.pitch as i32);
            let r = egui::Rect::from_min_max(
                egui::pos2(x0.max(grid_rect.left()) + 1.0, y + 1.0),
                egui::pos2(x1.min(grid_rect.right()) - 1.0, y + ROW_H - 1.0),
            );
            if r.width() < 2.0 {
                continue;
            }
            let selected = self.drag.map(|d| d.idx == idx).unwrap_or(false);
            let fill = if selected {
                theme.accent
            } else {
                theme.accent_hot
            };
            gp.rect_filled(
                r,
                egui::Rounding::same(2.0),
                fill.gamma_multiply(0.35 + 0.65 * n.vel.clamp(0.0, 1.0)),
            );
            gp.rect_stroke(r, egui::Rounding::same(2.0), egui::Stroke::new(1.0_f32, fill));
            if r.width() > 34.0 {
                gp.text(
                    r.left_center() + egui::vec2(4.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    note_name(n.pitch),
                    egui::FontId::proportional(9.0),
                    theme.cream,
                );
            }
        }

        // -- playhead ------------------------------------------------------
        let px = x_of(playhead);
        if px >= grid_rect.left() && px <= grid_rect.right() {
            gp.line_segment(
                [
                    egui::pos2(px, grid_rect.top()),
                    egui::pos2(px, grid_rect.bottom()),
                ],
                egui::Stroke::new(1.2_f32, theme.accent),
            );
        }

        // -- hints ---------------------------------------------------------
        painter.text(
            egui::pos2(grid_rect.right() - 8.0, rect.top() + 4.0),
            egui::Align2::RIGHT_TOP,
            format!(
                "click = add · drag = move · drag edge = length · right-click = delete · alt = free · {} note(s)",
                self.notes.len()
            ),
            egui::FontId::proportional(9.0),
            theme.dim,
        );
    }
}
