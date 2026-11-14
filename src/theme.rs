//! Color themes for LOSANGELES.
//!
//! A theme is a named palette of the colours the UI uses. Switching themes is
//! instant: the app just swaps the `Theme` and reinstalls the egui visuals.

use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Warm,
    Noir,
    Neon,
}

impl ThemeName {
    pub const ALL: [ThemeName; 3] = [ThemeName::Warm, ThemeName::Noir, ThemeName::Neon];

    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Warm => "warm · analog",
            ThemeName::Noir => "noir · graphite",
            ThemeName::Neon => "neon · chrome",
        }
    }
}

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    pub bg: egui::Color32,
    pub panel: egui::Color32,
    pub panel_hi: egui::Color32,
    pub lane: egui::Color32,
    pub lane_alt: egui::Color32,
    pub grid: egui::Color32,
    pub accent: egui::Color32,
    pub accent_hot: egui::Color32,
    pub cream: egui::Color32,
    pub dim: egui::Color32,
    pub red: egui::Color32,
}

impl Theme {
    pub fn get(name: ThemeName) -> Self {
        match name {
            ThemeName::Warm => Self {
                name: "warm · analog",
                bg: egui::Color32::from_rgb(0x14, 0x11, 0x0F),
                panel: egui::Color32::from_rgb(0x1B, 0x17, 0x14),
                panel_hi: egui::Color32::from_rgb(0x24, 0x1E, 0x19),
                lane: egui::Color32::from_rgb(0x1F, 0x1A, 0x16),
                lane_alt: egui::Color32::from_rgb(0x1C, 0x17, 0x14),
                grid: egui::Color32::from_rgb(0x31, 0x29, 0x24),
                accent: egui::Color32::from_rgb(0xE0, 0xA1, 0x5C),
                accent_hot: egui::Color32::from_rgb(0xC9, 0x7B, 0x3A),
                cream: egui::Color32::from_rgb(0xE8, 0xDC, 0xC8),
                dim: egui::Color32::from_rgb(0x7A, 0x6A, 0x5C),
                red: egui::Color32::from_rgb(0xD4, 0x5A, 0x3C),
            },
            ThemeName::Noir => Self {
                name: "noir · graphite",
                bg: egui::Color32::from_rgb(0x0E, 0x0E, 0x10),
                panel: egui::Color32::from_rgb(0x16, 0x16, 0x1A),
                panel_hi: egui::Color32::from_rgb(0x20, 0x20, 0x26),
                lane: egui::Color32::from_rgb(0x1A, 0x1A, 0x1F),
                lane_alt: egui::Color32::from_rgb(0x17, 0x17, 0x1C),
                grid: egui::Color32::from_rgb(0x2A, 0x2A, 0x32),
                accent: egui::Color32::from_rgb(0xA0, 0xB0, 0xFF),
                accent_hot: egui::Color32::from_rgb(0x6E, 0x82, 0xE8),
                cream: egui::Color32::from_rgb(0xE4, 0xE6, 0xF0),
                dim: egui::Color32::from_rgb(0x68, 0x6E, 0x7E),
                red: egui::Color32::from_rgb(0xE0, 0x5A, 0x5A),
            },
            ThemeName::Neon => Self {
                name: "neon · chrome",
                bg: egui::Color32::from_rgb(0x10, 0x0B, 0x18),
                panel: egui::Color32::from_rgb(0x1A, 0x13, 0x26),
                panel_hi: egui::Color32::from_rgb(0x26, 0x1B, 0x36),
                lane: egui::Color32::from_rgb(0x1E, 0x16, 0x2C),
                lane_alt: egui::Color32::from_rgb(0x1A, 0x13, 0x28),
                grid: egui::Color32::from_rgb(0x35, 0x28, 0x4A),
                accent: egui::Color32::from_rgb(0x46, 0xF0, 0xC0),
                accent_hot: egui::Color32::from_rgb(0x2E, 0xB8, 0x9A),
                cream: egui::Color32::from_rgb(0xE9, 0xE6, 0xF7),
                dim: egui::Color32::from_rgb(0x8E, 0x7E, 0xB0),
                red: egui::Color32::from_rgb(0xFF, 0x4D, 0x7E),
            },
        }
    }

    /// Install this palette as the egui style for the whole app.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();

        visuals.override_text_color = Some(self.cream);
        visuals.panel_fill = self.panel;
        visuals.window_fill = self.panel;
        visuals.extreme_bg_color = self.bg;
        visuals.faint_bg_color = self.lane;
        visuals.hyperlink_color = self.accent;
        visuals.selection.bg_fill = self.accent_hot;
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, self.cream);

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = self.panel;
        w.noninteractive.weak_bg_fill = self.panel;
        w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, self.grid);
        w.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, self.cream);
        w.noninteractive.rounding = egui::Rounding::same(2.0);

        w.inactive.bg_fill = self.panel_hi;
        w.inactive.weak_bg_fill = self.panel_hi;
        w.inactive.bg_stroke = egui::Stroke::new(1.0_f32, self.grid);
        w.inactive.fg_stroke = egui::Stroke::new(1.0_f32, self.cream);
        w.inactive.rounding = egui::Rounding::same(2.0);

        w.hovered.bg_fill = self.panel_hi.gamma_multiply(1.3);
        w.hovered.weak_bg_fill = self.panel_hi.gamma_multiply(1.3);
        w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, self.accent_hot);
        w.hovered.fg_stroke = egui::Stroke::new(1.0_f32, self.accent);
        w.hovered.rounding = egui::Rounding::same(2.0);

        w.active.bg_fill = self.accent_hot.gamma_multiply(0.55);
        w.active.weak_bg_fill = self.accent_hot.gamma_multiply(0.55);
        w.active.bg_stroke = egui::Stroke::new(1.0_f32, self.accent);
        w.active.fg_stroke = egui::Stroke::new(1.0_f32, self.cream);
        w.active.rounding = egui::Rounding::same(2.0);

        w.open.bg_fill = self.panel_hi;
        w.open.weak_bg_fill = self.panel_hi;
        w.open.bg_stroke = egui::Stroke::new(1.0_f32, self.grid);
        w.open.fg_stroke = egui::Stroke::new(1.0_f32, self.cream);
        w.open.rounding = egui::Rounding::same(2.0);

        let mut style = (*ctx.style()).clone();
        style.visuals = visuals;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.slider_width = 110.0;
        style.text_styles = [
            (egui::TextStyle::Heading, egui::FontId::proportional(18.0)),
            (egui::TextStyle::Body, egui::FontId::proportional(13.0)),
            (egui::TextStyle::Button, egui::FontId::proportional(12.0)),
            (egui::TextStyle::Small, egui::FontId::proportional(10.0)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(12.0)),
        ]
        .into();

        ctx.set_style(style);
    }
}
