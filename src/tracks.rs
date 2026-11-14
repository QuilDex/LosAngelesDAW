//! Track model: an unlimited list of tracks, each with clips, MIDI notes,
//! per-track effects and a plugin chain (CLAP / VST3 / VST).
//!
//! Hosting real CLAP / VST3 / VST binaries needs a native plugin runtime
//! (FFI to the plugin SDKs, e.g. `clack` / `vst3-sys`), which is not wired in
//! yet. A plugin is therefore modelled here as metadata + wet mix: the browser,
//! the chain order and the scanning work, the DSP hosting is the next stage.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use crate::decode::ClipData;
use crate::library::PluginKind;
use crate::pianoroll::Note;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Drive,
    Lowpass,
    Highpass,
    Tremolo,
}

impl EffectKind {
    pub const ALL: [EffectKind; 4] = [
        EffectKind::Drive,
        EffectKind::Lowpass,
        EffectKind::Highpass,
        EffectKind::Tremolo,
    ];
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Drive => "drive",
            EffectKind::Lowpass => "lowpass",
            EffectKind::Highpass => "highpass",
            EffectKind::Tremolo => "tremolo",
        }
    }
}

#[derive(Clone)]
pub struct Effect {
    pub kind: EffectKind,
    /// 0..1 intensity / cutoff / depth.
    pub amount: f32,
    pub enabled: bool,
}

impl Effect {
    pub fn new(kind: EffectKind) -> Self {
        Self {
            kind,
            amount: 0.5,
            enabled: true,
        }
    }
}

#[derive(Clone)]
pub struct PluginInst {
    pub name: String,
    pub path: PathBuf,
    pub kind: PluginKind,
    /// 0..1 wet mix (host-side, so it does something before DSP hosting lands).
    pub wet: f32,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct PlacedClip {
    pub data: Arc<ClipData>,
    pub track: usize,
    pub start: f32,
}

#[derive(Clone)]
pub struct Track {
    pub name: String,
    pub color: egui::Color32,
    pub volume: f32,
    pub muted: bool,
    pub solo: bool,
    pub effects: Vec<Effect>,
    pub plugins: Vec<PluginInst>,
    pub notes: Vec<Note>,
}

impl Track {
    pub fn new(name: &str, color: egui::Color32) -> Self {
        Self {
            name: name.to_string(),
            color,
            volume: 1.0,
            muted: false,
            solo: false,
            effects: Vec::new(),
            plugins: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// Palette used to colour fresh tracks.
pub const TRACK_COLORS: [egui::Color32; 8] = [
    egui::Color32::from_rgb(0xE0, 0xA1, 0x5C), // amber
    egui::Color32::from_rgb(0x9E, 0xC1, 0x8C), // sage
    egui::Color32::from_rgb(0x9F, 0xB3, 0xE0), // slate blue
    egui::Color32::from_rgb(0xE0, 0x8C, 0x9E), // rose
    egui::Color32::from_rgb(0xC9, 0x9C, 0xE0), // lavender
    egui::Color32::from_rgb(0x9C, 0xD8, 0xD4), // teal
    egui::Color32::from_rgb(0xE0, 0xC0, 0x7C), // gold
    egui::Color32::from_rgb(0x8C, 0xB4, 0x8C), // moss
];
