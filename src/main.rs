//! LOSANGELES — experimental grid-free DAW.
//!
//! Stage 3: left browser with sound packs and plugins, unlimited tracks with
//! per-track effects and plugin chains, switchable themes, a piano roll and a
//! working "insert sound" path (click a sample, add a folder, or drop files).
//!
//! Module layout:
//!   src/decode.rs     — audio decoding + waveform peaks
//!   src/engine.rs     — cpal stream, mixer, effects, note voices
//!   src/library.rs    — pack scanning, plugin discovery
//!   src/tracks.rs     — track model (unlimited), effects, plugin instances
//!   src/theme.rs      — colour themes
//!   src/pianoroll.rs  — note editor

mod decode;
mod engine;
mod library;
mod pianoroll;
mod theme;
mod tracks;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use decode::ClipData;
use engine::{AudioEngine, Command, MixSnapshot, PlacedNote, Shared, TrackMix};
use library::{default_plugin_dirs, PluginEntry, PluginKind};
use pianoroll::PianoRoll;
use theme::{Theme, ThemeName};
use tracks::{EffectKind, PlacedClip, PluginInst, Track, TRACK_COLORS};

// Timeline geometry
const RULER_H: f32 = 26.0;
const TRACK_H: f32 = 92.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BrowserTab {
    Sounds,
    Plugins,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bottom {
    Closed,
    Piano,
    Fx,
}

#[derive(Clone, Copy)]
struct ClipDrag {
    idx: usize,
    offset: f32,
}

struct LosAngelesApp {
    shared: Arc<Shared>,
    engine: Option<AudioEngine>,
    engine_error: Option<String>,

    playing: bool,
    tone: bool,
    gain: f32,
    meter: f32,

    zoom: f32,   // pixels per second
    scroll: f32, // leftmost visible second

    tracks: Vec<Track>,
    selected: usize,
    clips: Vec<PlacedClip>,

    theme_name: ThemeName,
    theme: Theme,

    tab: BrowserTab,
    packs: Vec<library::Pack>,
    pack_open: Vec<bool>,
    sound_dir: String,
    plugins: Vec<PluginEntry>,
    plugin_dir: String,

    bottom: Bottom,
    roll: PianoRoll,
    roll_track: usize,

    clip_drag: Option<ClipDrag>,
    notice: Option<String>,
    notice_bad: bool,
    dirty: bool,
}

impl LosAngelesApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let theme_name = ThemeName::Warm;
        let theme = Theme::get(theme_name);
        theme.apply(&cc.egui_ctx);

        let shared = Arc::new(Shared::default());
        Shared::store_f32(&shared.gain, 0.8);

        let mut tracks = Vec::new();
        for i in 0..4 {
            tracks.push(Track::new(
                &format!("TRACK {:02}", i + 1),
                TRACK_COLORS[i % TRACK_COLORS.len()],
            ));
        }

        let mut app = Self {
            shared,
            engine: None,
            engine_error: None,
            playing: false,
            tone: false,
            gain: 0.8,
            meter: 0.0,
            zoom: 90.0,
            scroll: 0.0,
            tracks,
            selected: 0,
            clips: Vec::new(),
            theme_name,
            theme,
            tab: BrowserTab::Sounds,
            packs: Vec::new(),
            pack_open: Vec::new(),
            sound_dir: String::new(),
            plugins: Vec::new(),
            plugin_dir: String::new(),
            bottom: Bottom::Piano,
            roll: PianoRoll::default(),
            roll_track: 0,
            clip_drag: None,
            notice: None,
            notice_bad: false,
            dirty: true,
        };

        app.scan_packs_dir(PathBuf::from("packs"));
        app.scan_plugins_dirs();
        app.init_audio();
        app
    }

    // -- setup / scanning --------------------------------------------------

    fn scan_packs_dir(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            self.notice = Some(format!(
                "{} not found — app-folder `packs/` is scanned on start",
                dir.display()
            ));
            self.notice_bad = true;
            return;
        }
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && !self.packs.iter().any(|p| p.root == path) {
                    found.push(path);
                }
            }
        }
        found.sort();
        let added = found.len();
        for path in found {
            self.packs.push(library::Pack::scan(path));
            self.pack_open.push(false);
        }
        if added > 0 {
            self.notice = Some(format!("{added} pack(s) from {}", dir.display()));
            self.notice_bad = false;
        }
    }

    fn add_pack_or_file(&mut self, path: PathBuf) {
        if !path.exists() {
            self.notice = Some(format!("{}: not found", path.display()));
            self.notice_bad = true;
            return;
        }
        if path.is_dir() {
            self.packs.push(library::Pack::scan(path));
            self.pack_open.push(true);
            let name = self.packs.last().map(|p| p.name.clone()).unwrap_or_default();
            let count = self.packs.last().map(|p| p.sounds.len()).unwrap_or(0);
            self.notice = Some(format!("pack {name} · {count} sample(s)"));
            self.notice_bad = count == 0;
        } else if library::is_audio(&path) {
            self.insert_audio(path);
        } else {
            self.notice = Some("need a folder of samples or an audio file".into());
            self.notice_bad = true;
        }
    }

    fn scan_plugins_dirs(&mut self) {
        self.plugins.clear();
        for dir in default_plugin_dirs() {
            if dir.is_dir() {
                library::scan_plugins(&dir, &mut self.plugins);
            }
        }
        self.plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.plugins.dedup_by(|a, b| a.path == b.path);
    }

    fn scan_plugin_dir(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            self.notice = Some(format!("{}: folder not found", dir.display()));
            self.notice_bad = true;
            return;
        }
        let before = self.plugins.len();
        library::scan_plugins(&dir, &mut self.plugins);
        self.plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.plugins.dedup_by(|a, b| a.path == b.path);
        let added = self.plugins.len().saturating_sub(before);
        self.notice = Some(format!("{added} plugin(s) from {}", dir.display()));
        self.notice_bad = added == 0;
    }

    // -- audio plumbing ----------------------------------------------------

    fn init_audio(&mut self) {
        match AudioEngine::start(self.shared.clone()) {
            Ok(mut engine) => {
                engine.send(Command::SetGain(self.gain));
                engine.send(Command::SetTone(self.tone));
                engine.send(Command::SetMix(Box::new(self.mix_snapshot())));
                self.engine = Some(engine);
                self.engine_error = None;
                self.notice = Some("audio engine online".into());
                self.notice_bad = false;
            }
            Err(e) => {
                self.engine = None;
                self.engine_error = Some(e.to_string());
            }
        }
    }

    fn mix_snapshot(&self) -> MixSnapshot {
        let any_solo = self.tracks.iter().any(|t| t.solo);
        let mut mix_tracks = Vec::with_capacity(self.tracks.len());
        for t in &self.tracks {
            let (mut drive, mut lowpass, mut highpass, mut tremolo) = (0.0f32, 0.0, 0.0, 0.0);
            for e in t.effects.iter().filter(|e| e.enabled) {
                match e.kind {
                    EffectKind::Drive => drive = drive.max(e.amount),
                    EffectKind::Lowpass => lowpass = lowpass.max(e.amount),
                    EffectKind::Highpass => highpass = highpass.max(e.amount),
                    EffectKind::Tremolo => tremolo = tremolo.max(e.amount),
                }
            }
            let audible = !t.muted && (!any_solo || t.solo);
            mix_tracks.push(TrackMix {
                volume: if audible { t.volume.clamp(0.0, 2.0) } else { 0.0 },
                drive,
                lowpass,
                highpass,
                tremolo,
            });
        }

        let notes = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| t.notes.iter().map(move |n| PlacedNote { track: ti, note: *n }))
            .collect();

        MixSnapshot {
            tracks: mix_tracks,
            clips: self.clips.clone(),
            notes,
        }
    }

    fn sync_mix(&mut self) {
        let snapshot = self.mix_snapshot();
        if let Some(engine) = self.engine.as_mut() {
            engine.send(Command::SetMix(Box::new(snapshot)));
        }
    }

    fn set_playing(&mut self, on: bool) {
        if self.engine.is_none() {
            self.notice = Some("audio offline — press RETRY in the status bar".into());
            self.notice_bad = true;
            return;
        }
        if on {
            self.sync_mix();
        }
        self.playing = on;
        if let Some(engine) = self.engine.as_mut() {
            engine.send(if on { Command::Play } else { Command::Stop });
        }
    }

    fn set_gain(&mut self, g: f32) {
        self.gain = g;
        Shared::store_f32(&self.shared.gain, g);
        if let Some(engine) = self.engine.as_mut() {
            engine.send(Command::SetGain(g));
        }
    }

    fn set_tone(&mut self, on: bool) {
        self.tone = on;
        self.shared.tone.store(on, Ordering::Relaxed);
        if let Some(engine) = self.engine.as_mut() {
            engine.send(Command::SetTone(on));
        }
    }

    fn position_seconds(&self) -> f32 {
        let sr = self
            .engine
            .as_ref()
            .map(|e| e.info.sample_rate)
            .unwrap_or(48_000)
            .max(1);
        self.shared.track_frames.load(Ordering::Relaxed) as f32 / sr as f32
    }

    fn poll_meter(&mut self) {
        let peak = Shared::load_f32(&self.shared.peak);
        self.meter = (self.meter * 0.86).max(peak);
    }

    // -- tracks ------------------------------------------------------------

    fn add_track(&mut self) {
        let i = self.tracks.len();
        let color = TRACK_COLORS[i % TRACK_COLORS.len()];
        self.tracks
            .push(Track::new(&format!("TRACK {:02}", i + 1), color));
        self.selected = i;
        self.dirty = true;
    }

    fn remove_track(&mut self, idx: usize) {
        if self.tracks.len() <= 1 || idx >= self.tracks.len() {
            return;
        }
        self.tracks.remove(idx);
        for clip in self.clips.iter_mut() {
            if clip.track == idx {
                clip.track = usize::MAX;
            } else if clip.track > idx {
                clip.track -= 1;
            }
        }
        self.clips.retain(|c| c.track != usize::MAX);
        self.selected = self.selected.min(self.tracks.len() - 1);
        self.roll_track = self.selected;
        self.dirty = true;
    }

    fn move_track(&mut self, idx: usize, up: bool) {
        let other = if up {
            match idx.checked_sub(1) {
                Some(o) => o,
                None => return,
            }
        } else if idx + 1 < self.tracks.len() {
            idx + 1
        } else {
            return;
        };
        self.tracks.swap(idx, other);
        for clip in self.clips.iter_mut() {
            if clip.track == idx {
                clip.track = other;
            } else if clip.track == other {
                clip.track = idx;
            }
        }
        self.selected = other;
        self.dirty = true;
    }

    fn select_track(&mut self, idx: usize) {
        if idx < self.tracks.len() && idx != self.selected {
            self.selected = idx;
            self.dirty = true;
        }
    }

    // -- insertion ---------------------------------------------------------

    /// Append after the last clip when stopped, or at the playhead when playing.
    fn insert_time_for(&self, track: usize) -> f32 {
        if self.playing {
            return self.position_seconds().max(0.0);
        }
        self.clips
            .iter()
            .filter(|c| c.track == track)
            .map(|c| c.start + c.data.duration)
            .fold(0.0f32, f32::max)
    }

    fn insert_audio(&mut self, path: PathBuf) {
        let Some(last) = self.tracks.len().checked_sub(1) else {
            return;
        };
        let track = self.selected.min(last);
        let start = self.insert_time_for(track);
        match ClipData::decode(&path) {
            Ok(data) => {
                let name = data.name.clone();
                let duration = data.duration;
                self.clips.push(PlacedClip {
                    data: Arc::new(data),
                    track,
                    start,
                });
                self.notice = Some(format!(
                    "{name} · {duration:.2}s → {} @ {start:.2}s",
                    self.tracks[track].name
                ));
                self.notice_bad = false;
                self.dirty = true;
            }
            Err(e) => {
                self.notice = Some(format!("{}: {e}", path.display()));
                self.notice_bad = true;
            }
        }
    }

    fn add_files_at(&mut self, files: Vec<PathBuf>, track: usize, time: f32) {
        let Some(last) = self.tracks.len().checked_sub(1) else {
            return;
        };
        let track = track.min(last);
        let mut t = time.max(0.0);
        let mut loaded = 0usize;
        let mut first_error: Option<String> = None;

        for path in files {
            if !library::is_audio(&path) {
                // A dropped folder is a pack, not a clip.
                if path.is_dir() {
                    self.packs.push(library::Pack::scan(path));
                    self.pack_open.push(true);
                    loaded += 1;
                    continue;
                }
                first_error.get_or_insert_with(|| {
                    format!(
                        "{}: not an audio file",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )
                });
                continue;
            }
            match ClipData::decode(&path) {
                Ok(data) => {
                    let duration = data.duration;
                    self.clips.push(PlacedClip {
                        data: Arc::new(data),
                        track,
                        start: t,
                    });
                    t += duration;
                    loaded += 1;
                }
                Err(e) => {
                    first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
                }
            }
        }

        if let Some(err) = first_error {
            self.notice = Some(err);
            self.notice_bad = true;
        } else if loaded > 0 {
            self.notice = Some(format!("{loaded} item(s) → {}", self.tracks[track].name));
            self.notice_bad = false;
            self.dirty = true;
        }
    }

    fn add_plugin_to_selected(&mut self, entry: &PluginEntry) {
        let Some(last) = self.tracks.len().checked_sub(1) else {
            return;
        };
        let idx = self.selected.min(last);
        self.tracks[idx].plugins.push(PluginInst {
            name: entry.name.clone(),
            path: entry.path.clone(),
            kind: entry.kind,
            wet: 1.0,
            enabled: true,
        });
        self.notice = Some(format!(
            "{} ({}) → {}",
            entry.name,
            entry.kind.label(),
            self.tracks[idx].name
        ));
        self.notice_bad = false;
        self.dirty = true;
    }
}
impl LosAngelesApp {
    // -- top bar -----------------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        let mut cycle_theme = false;
        let accent = theme.accent;
        let dim = theme.dim;
        let cream = theme.cream;
        let red = theme.red;
        let mut add_track = false;
        let mut toggle_bottom: Option<Bottom> = None;

        egui::TopBottomPanel::top("transport")
            .exact_height(62.0)
            .frame(
                egui::Frame::none()
                    .fill(theme.panel)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("LOSANGELES")
                                .size(19.0)
                                .strong()
                                .color(accent),
                        );
                        ui.label(
                            egui::RichText::new("grid-free  ·  off-grid groove")
                                .size(10.0)
                                .color(dim),
                        );
                    });

                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(6.0);

                    if ui
                        .selectable_label(self.playing, egui::RichText::new("PLAY").size(12.0))
                        .clicked()
                    {
                        self.set_playing(!self.playing);
                    }
                    if ui
                        .selectable_label(!self.playing, egui::RichText::new("STOP").size(12.0))
                        .clicked()
                    {
                        self.set_playing(false);
                    }

                    ui.add_space(4.0);
                    let tone = ui
                        .selectable_label(
                            self.tone,
                            egui::RichText::new("TONE")
                                .size(11.0)
                                .color(if self.tone { accent } else { dim }),
                        )
                        .on_hover_text("440 Hz reference tone — press PLAY to test the output");
                    if tone.clicked() {
                        self.set_tone(!self.tone);
                    }

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("MASTER").size(10.0).color(dim));
                    let mut gain = self.gain;
                    if ui
                        .add(
                            egui::Slider::new(&mut gain, 0.0..=1.2)
                                .show_value(false)
                                .desired_width(92.0),
                        )
                        .changed()
                    {
                        self.set_gain(gain);
                    }

                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(54.0, 8.0), egui::Sense::hover());
                    let painter = ui.painter();
                    painter.rect_filled(rect, egui::Rounding::same(2.0), theme.panel_hi);
                    let width = rect.width() * self.meter.clamp(0.0, 1.0);
                    if width > 0.0 {
                        let bar =
                            egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()));
                        let color = if self.meter > 0.9 { red } else { theme.accent_hot };
                        painter.rect_filled(bar, egui::Rounding::same(2.0), color);
                    }

                    ui.add_space(10.0);
                    if ui.small_button("+ TRACK").clicked() {
                        add_track = true;
                    }
                    if ui
                        .selectable_label(self.bottom == Bottom::Fx, "FX")
                        .on_hover_text("track effects and plugin chain")
                        .clicked()
                    {
                        toggle_bottom = Some(Bottom::Fx);
                    }
                    if ui
                        .selectable_label(self.bottom == Bottom::Piano, "PIANO")
                        .on_hover_text("piano roll for the selected track")
                        .clicked()
                    {
                        toggle_bottom = Some(Bottom::Piano);
                    }
                    if ui
                        .small_button("THEME")
                        .on_hover_text("cycle colour theme")
                        .clicked()
                    {
                        cycle_theme = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let pos = self.position_seconds();
                        let mins = (pos / 60.0).floor() as u32;
                        let secs = pos - mins as f32 * 60.0;
                        ui.label(
                            egui::RichText::new(format!("{mins:02}:{secs:05.2}"))
                                .monospace()
                                .size(16.0)
                                .color(cream),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(self.theme_name.label())
                                .size(10.0)
                                .color(theme.accent_hot),
                        );
                    });
                });
            });

        if add_track {
            self.add_track();
        }
        if let Some(target) = toggle_bottom {
            self.bottom = if self.bottom == target {
                Bottom::Closed
            } else {
                target
            };
        }
        if cycle_theme {
            let all = ThemeName::ALL;
            let pos = all
                .iter()
                .position(|t| *t == self.theme_name)
                .unwrap_or(0);
            self.theme_name = all[(pos + 1) % all.len()];
            self.theme = Theme::get(self.theme_name);
            self.theme.apply(ctx);
        }
    }

    // -- status bar --------------------------------------------------------

    fn status_bar(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        let mut retry = false;

        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(
                egui::Frame::none()
                    .fill(theme.panel)
                    .inner_margin(egui::Margin::symmetric(14.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    match &self.engine {
                        Some(engine) => {
                            let info = &engine.info;
                            ui.label(egui::RichText::new("● AUDIO").size(10.0).color(theme.accent));
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {} · {} Hz · {} ch · {} · {:.1} ms",
                                    info.host,
                                    info.device,
                                    info.sample_rate,
                                    info.channels,
                                    if info.buffer_frames == 0 {
                                        "default buffer".to_string()
                                    } else {
                                        format!("{} frames", info.buffer_frames)
                                    },
                                    info.latency_ms(),
                                ))
                                .size(10.0)
                                .color(theme.dim),
                            );
                            let errs = self.shared.stream_errors.load(Ordering::Relaxed);
                            if errs > 0 {
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(format!("stream errors: {errs}"))
                                        .size(10.0)
                                        .color(theme.red),
                                );
                            }
                        }
                        None => {
                            ui.label(
                                egui::RichText::new("● AUDIO OFFLINE")
                                    .size(10.0)
                                    .color(theme.red),
                            );
                            if let Some(err) = &self.engine_error {
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(err).size(10.0).color(theme.dim));
                            }
                            ui.add_space(6.0);
                            if ui.small_button("RETRY").clicked() {
                                retry = true;
                            }
                        }
                    }

                    ui.add_space(10.0);
                    if let Some(note) = &self.notice {
                        let color = if self.notice_bad {
                            theme.red
                        } else {
                            theme.accent_hot
                        };
                        ui.label(egui::RichText::new(note).size(10.0).color(color));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} tracks · {} clips · unlimited · space = play",
                                self.tracks.len(),
                                self.clips.len()
                            ))
                            .size(10.0)
                            .color(theme.dim),
                        );
                    });
                });
            });

        if retry {
            self.init_audio();
        }
    }

    // -- left browser ------------------------------------------------------

    fn browser_panel(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        let mut tab = self.tab;

        egui::SidePanel::left("browser")
            .resizable(true)
            .default_width(272.0)
            .min_width(210.0)
            .frame(
                egui::Frame::none()
                    .fill(theme.panel)
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(tab == BrowserTab::Sounds, "SOUNDS")
                        .clicked()
                    {
                        tab = BrowserTab::Sounds;
                    }
                    if ui
                        .selectable_label(tab == BrowserTab::Plugins, "PLUGINS")
                        .clicked()
                    {
                        tab = BrowserTab::Plugins;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(match tab {
                                BrowserTab::Sounds => format!("{} packs", self.packs.len()),
                                BrowserTab::Plugins => format!("{} units", self.plugins.len()),
                            })
                            .size(10.0)
                            .color(theme.dim),
                        );
                    });
                });
                ui.separator();

                match tab {
                    BrowserTab::Sounds => self.sounds_tab(ui, &theme),
                    BrowserTab::Plugins => self.plugins_tab(ui, &theme),
                }
            });

        self.tab = tab;
    }

    fn sounds_tab(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let mut insert: Option<PathBuf> = None;
        let mut swap: Option<(usize, usize)> = None;
        let mut drop_pack: Option<usize> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for i in 0..self.packs.len() {
                    let name = self.packs[i].name.clone();
                    let count = self.packs[i].sounds.len();
                    let open = self.pack_open.get(i).copied().unwrap_or(false);

                    ui.horizontal(|ui| {
                        let head = ui.add(
                            egui::Button::new(
                                egui::RichText::new(format!(
                                    "{} {name}",
                                    if open { "v" } else { ">" }
                                ))
                                .size(12.0),
                            )
                            .fill(theme.panel_hi),
                        );
                        if head.clicked() {
                            if let Some(slot) = self.pack_open.get_mut(i) {
                                *slot = !open;
                            }
                        }
                        ui.label(egui::RichText::new(format!("{count}")).size(10.0).color(theme.dim));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("x").on_hover_text("remove pack").clicked() {
                                drop_pack = Some(i);
                            }
                            if ui.small_button("dn").clicked() && i + 1 < self.packs.len() {
                                swap = Some((i, i + 1));
                            }
                            if ui.small_button("up").clicked() && i > 0 {
                                swap = Some((i, i - 1));
                            }
                        });
                    });

                    if open {
                        ui.indent(format!("pack{i}"), |ui| {
                            for sound in &self.packs[i].sounds {
                                let resp = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("· {}", sound.name))
                                            .size(11.0)
                                            .color(theme.cream),
                                    )
                                    .sense(egui::Sense::click()),
                                );
                                if resp.clicked() {
                                    insert = Some(sound.path.clone());
                                }
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                resp.on_hover_text("click = insert on the selected track");
                            }
                            if self.packs[i].sounds.is_empty() {
                                ui.label(
                                    egui::RichText::new("no audio files inside")
                                        .size(10.0)
                                        .color(theme.dim),
                                );
                            }
                        });
                    }
                }

                if self.packs.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "No packs yet.\nFolders of samples placed in `packs/` next to the app are scanned on start, or add one below.",
                        )
                        .size(11.0)
                        .color(theme.dim),
                    );
                }
            });

        ui.separator();
        ui.label(egui::RichText::new("ADD PACK FOLDER").size(9.0).color(theme.dim));
        ui.add(
            egui::TextEdit::singleline(&mut self.sound_dir)
                .hint_text("C:\\samples\\my pack")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let path = self.sound_dir.trim().to_string();
            if ui.button("ADD").clicked() && !path.is_empty() {
                self.add_pack_or_file(PathBuf::from(path));
            }
            if ui.button("RESCAN packs/").clicked() {
                self.scan_packs_dir(PathBuf::from("packs"));
            }
        });

        if let Some(path) = insert {
            self.insert_audio(path);
        }
        if let Some((a, b)) = swap {
            self.packs.swap(a, b);
            self.pack_open.swap(a, b);
        }
        if let Some(i) = drop_pack {
            self.packs.remove(i);
            if i < self.pack_open.len() {
                self.pack_open.remove(i);
            }
        }
    }

    fn plugins_tab(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let mut add: Option<PluginEntry> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.plugins.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No CLAP / VST3 / VST found in the system folders.\nPoint the app at your plugin folder below and press SCAN.",
                        )
                        .size(11.0)
                        .color(theme.dim),
                    );
                }
                for entry in &self.plugins {
                    ui.horizontal(|ui| {
                        let badge = match entry.kind {
                            PluginKind::Clap => theme.accent,
                            PluginKind::Vst3 => theme.accent_hot,
                            PluginKind::Vst => theme.cream,
                        };
                        ui.label(
                            egui::RichText::new(entry.kind.label())
                                .size(9.0)
                                .monospace()
                                .color(badge),
                        );
                        let resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&entry.name).size(11.0).color(theme.cream),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if resp.clicked() {
                            add = Some(entry.clone());
                        }
                        resp.on_hover_text(format!(
                            "{}\nclick = add to the selected track",
                            entry.path.display()
                        ));
                    });
                }
            });

        ui.separator();
        ui.label(egui::RichText::new("SCAN A PLUGIN FOLDER").size(9.0).color(theme.dim));
        ui.add(
            egui::TextEdit::singleline(&mut self.plugin_dir)
                .hint_text("D:\\VST3")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let path = self.plugin_dir.trim().to_string();
            if ui.button("SCAN").clicked() && !path.is_empty() {
                self.scan_plugin_dir(PathBuf::from(path));
            }
            if ui.button("SYSTEM").clicked() {
                self.scan_plugins_dirs();
                self.notice = Some(format!("{} plugin(s) in system folders", self.plugins.len()));
                self.notice_bad = false;
            }
        });

        if let Some(entry) = add {
            self.add_plugin_to_selected(&entry);
        }
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Plugins are listed and chained per track. Hosting their DSP (loading the binary) needs a native plugin runtime — the next stage.",
            )
            .size(9.0)
            .color(theme.dim),
        );
    }
}
// __PART_C__

