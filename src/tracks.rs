//! Track model: an unlimited list of tracks, each with clips, per-track effects
//! and a plugin chain (CLAP / VST3 / VST).
//!
//! Hosting real CLAP / VST3 / VST binaries requires a native plugin runtime
//! (e.g. `clack` / `vst3-sys` FFI). That crate has not been wired in yet, so a
//! plugin is modelled here as metadata plus a set of "wet/dry" style parameters;
//! the audio engine can be extended later to run actual plugin DSP.

use std::path::PathBuf;

use crate::library::PluginKinddied;
use crate::decode::ClipData;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Drive,
    Lowpass,
    Highpass,
    Chorus,
}

impl EffectKind {
    pub const ALL: [EffectKind; 4] = [
        EffectKind::Drive,
        EffectKind::Lowpass,
        EffectKind::Highpass,
        EffectKind::Chorus,
    ];
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Drive => "drive",
            EffectKind::Lowpass => "lowpass",
            EffectKind::Highpass => "highpass",
            EffectKind::Chorus => "chorus",
        }
    }
}

#[derive(Clone)]
pub struct Effect {
    pub kind: EffectKind,
    /// 0..1 intensity / cutoff / mix.
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
    pub kind: crate::library::PluginKind,
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
        }
    }
}

/// Palette used to colour fresh tracks (kept in sync with the warm theme).
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