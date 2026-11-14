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
//!   src/mixrack.rs    — mix rack: master + one strip per track, insert slots
//!   src/tracks.rs     — track model (unlimited), effects, plugin instances
//!   src/theme.rs      — colour themes
//!   src/pianoroll.rs  — note editor

mod decode;
mod engine;
mod library;
mod mixrack;
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
use engine::{AudioEngine, Command, InputEngine, MixSnapshot, PlacedNote, Shared, TrackMix};
use library::{default_plugin_dirs, PluginEntry, PluginKind};
use pianoroll::PianoRoll;
use theme::{Theme, ThemeName};
use tracks::{EffectKind, Master, PlacedClip, PluginInst, Track, TRACK_COLORS};

// Timeline geometry
const RULER_H: f32 = 26.0;
const TRACK_H: f32 = 92.0;
const LANE_GAP: f32 = 2.0;

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
    Mix,
}

#[derive(Clone, Copy)]
struct ClipDrag {
    idx: usize,
    offset: f32,
}

/// A sound dragged out of the browser towards the timeline. `pos` lags the
/// pointer a little so the ghost glides instead of jumping frame by frame.
struct SoundDrag {
    data: Arc<ClipData>,
    pos: egui::Pos2,
}

struct LosAngelesApp {
    shared: Arc<Shared>,
    engine: Option<AudioEngine>,
    engine_error: Option<String>,

    input: Option<InputEngine>,
    input_error: Option<String>,
    input_peak: f32,

    output_devices: Vec<String>,
    input_devices: Vec<String>,
    output_choice: Option<String>,
    input_choice: Option<String>,
    show_settings: bool,

    playing: bool,
    tone: bool,
    gain: f32,
    meter: f32,

    zoom: f32,    // pixels per second
    scroll: f32,  // leftmost visible second
    vscroll: f32, // vertical track scroll in pixels

    tracks: Vec<Track>,
    master: Master,
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
    sound_drag: Option<SoundDrag>,
    clip_cache: Vec<(PathBuf, Arc<ClipData>)>,
    lane_rect: egui::Rect,
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
            input: None,
            input_error: None,
            input_peak: 0.0,
            output_devices: Vec::new(),
            input_devices: Vec::new(),
            output_choice: None,
            input_choice: None,
            show_settings: false,
            playing: false,
            tone: false,
            gain: 0.8,
            meter: 0.0,
            zoom: 90.0,
            scroll: 0.0,
            vscroll: 0.0,
            tracks,
            master: Master::default(),
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
            sound_drag: None,
            clip_cache: Vec::new(),
            lane_rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.0, 0.0)),
            notice: None,
            notice_bad: false,
            dirty: true,
        };

        app.scan_packs_dir(PathBuf::from("packs"));
        app.scan_plugins_dirs();
        app.refresh_devices();
        app.init_audio();
        app.init_input();
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

    fn refresh_devices(&mut self) {
        self.output_devices = engine::output_devices();
        self.input_devices = engine::input_devices();
    }

    fn init_input(&mut self) {
        match InputEngine::start(self.shared.clone(), self.input_choice.as_deref()) {
            Ok(input) => {
                self.input = Some(input);
                self.input_error = None;
            }
            Err(e) => {
                self.input = None;
                self.input_error = Some(e.to_string());
            }
        }
    }

    /// Drop the current output stream and open the chosen device.
    fn set_output_device(&mut self, choice: Option<String>) {
        self.output_choice = choice;
        self.engine = None;
        self.playing = false;
        self.init_audio();
    }

    /// Drop the current input stream and open the chosen microphone.
    fn set_input_device(&mut self, choice: Option<String>) {
        self.input_choice = choice;
        self.input = None;
        self.input_peak = 0.0;
        self.init_input();
    }

    fn poll_input(&mut self) {
        let peak = Shared::load_f32(&self.shared.input_peak);
        self.input_peak = (self.input_peak * 0.86).max(peak);
    }

    fn init_audio(&mut self) {
        match AudioEngine::start_on(self.shared.clone(), self.output_choice.as_deref()) {
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
            let (mut drive, mut lowpass, mut highpass, mut tremolo) =
                (0.0f32, 0.0f32, 0.0f32, 0.0f32);
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

    /// Decoded clips are kept around: preview, the drag ghost and the drop all
    /// reuse one decode of the same file.
    fn cached_clip(&mut self, path: &std::path::Path) -> Option<Arc<ClipData>> {
        if let Some((_, data)) = self.clip_cache.iter().find(|(p, _)| p == path) {
            return Some(data.clone());
        }
        match ClipData::decode(path) {
            Ok(data) => {
                let data = Arc::new(data);
                self.clip_cache.push((path.to_path_buf(), data.clone()));
                if self.clip_cache.len() > 32 {
                    self.clip_cache.remove(0);
                }
                Some(data)
            }
            Err(e) => {
                self.clip_cache.push((path.to_path_buf(), Arc::new(ClipData {
                    name: "unreadable".to_string(),
                    samples: Vec::new(),
                    sample_rate: 48_000,
                    duration: 0.0,
                    peaks: Vec::new(),
                })));
                self.notice = Some(format!("{}: {e}", path.display()));
                self.notice_bad = true;
                None
            }
        }
    }

    fn preview_sound(&mut self, path: &std::path::Path) {
        let Some(data) = self.cached_clip(path) else {
            return;
        };
        let name = data.name.clone();
        let duration = data.duration;
        if let Some(engine) = self.engine.as_mut() {
            engine.preview(Some(data));
        } else {
            self.notice = Some("audio offline — no preview".into());
            self.notice_bad = true;
            return;
        }
        self.notice = Some(format!("preview · {name} · {duration:.2}s"));
        self.notice_bad = false;
    }

    fn place_clip(&mut self, data: Arc<ClipData>, track: usize, start: f32) {
        let Some(last) = self.tracks.len().checked_sub(1) else {
            return;
        };
        let track = track.min(last);
        let name = data.name.clone();
        let duration = data.duration;
        let start = start.max(0.0);
        self.clips.push(PlacedClip { data, track, start });
        self.notice = Some(format!(
            "{name} · {duration:.2}s → {} @ {start:.2}s",
            self.tracks[track].name
        ));
        self.notice_bad = false;
        self.sync_mix();
        self.dirty = true;
    }

    /// Ruler grid step in seconds, shared by the drawn ruler and the snapping.
    fn snap_step(&self) -> f32 {
        let mut s = 1.0f32;
        while s * self.zoom < 60.0 {
            s *= 2.0;
        }
        s
    }

    /// Nearest ruler line to the pointer: the box lands on a real grid line,
    /// not at a random offset, until the pointer leaves the lane.
    fn snapped_time(&self, lane: &egui::Rect, x: f32) -> f32 {
        let t = self.x_to_time(lane, x.max(lane.left())).max(0.0);
        let step = self.snap_step();
        ((t / step).round() * step).max(0.0)
    }

    /// Sound dragged from the browser: the ghost follows the pointer with a
    /// little inertia, snaps to the line under it, and lands on release.
    fn drag_and_drop_sound(&mut self, ctx: &egui::Context) {
        let Some(mut drag) = self.sound_drag.take() else {
            return;
        };
        let Some(pos) = ctx.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.latest_pos()))
        else {
            self.sound_drag = Some(drag);
            return;
        };

        drag.pos += (pos - drag.pos) * 0.5;
        let lane = self.lane_rect;
        let over = lane.contains(drag.pos);
        let time = self.snapped_time(&lane, drag.pos.x);
        let track = if self.tracks.is_empty() {
            0
        } else {
            let y = drag.pos.y.clamp(lane.top(), lane.bottom() - 1.0);
            let local = (y - lane.top() + self.vscroll).max(0.0);
            ((local / (TRACK_H + LANE_GAP)).floor() as usize).min(self.tracks.len() - 1)
        };

        let theme = self.theme.clone();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("sound-drag"),
        ));

        if over {
            let x = self.time_to_x(&lane, time);
            let w = (drag.data.duration * self.zoom).max(24.0);
            let top = lane.top() + track as f32 * (TRACK_H + LANE_GAP) - self.vscroll;
            let color = self.tracks.get(track).map(|t| t.color).unwrap_or(theme.accent);
            let ghost = egui::Rect::from_min_size(
                egui::pos2(x, top + 8.0),
                egui::vec2(w, TRACK_H - 16.0),
            );
            painter.rect_filled(
                ghost,
                egui::Rounding::same(3.0),
                color.gamma_multiply(0.30),
            );
            painter.rect_stroke(
                ghost,
                egui::Rounding::same(3.0),
                egui::Stroke::new(1.0_f32, color),
            );
            painter.line_segment(
                [egui::pos2(x, lane.top()), egui::pos2(x, lane.bottom())],
                egui::Stroke::new(1.2_f32, theme.accent),
            );
            painter.text(
                ghost.left_top() + egui::vec2(5.0, 4.0),
                egui::Align2::LEFT_TOP,
                &drag.data.name,
                egui::FontId::proportional(10.0),
                theme.cream,
            );
        }

        let label = egui::Rect::from_min_size(
            drag.pos + egui::vec2(14.0, -10.0),
            egui::vec2(190.0, 22.0),
        );
        painter.rect_filled(label, egui::Rounding::same(3.0), theme.panel_hi);
        painter.rect_stroke(
            label,
            egui::Rounding::same(3.0),
            egui::Stroke::new(1.0_f32, theme.accent),
        );
        painter.text(
            label.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            format!("{} · {:.2}s", drag.data.name, drag.data.duration),
            egui::FontId::proportional(10.0),
            theme.cream,
        );

        if ctx.input(|i| i.pointer.primary_down()) {
            ctx.request_repaint();
            self.sound_drag = Some(drag);
            return;
        }
        if over {
            self.place_clip(drag.data, track, time);
        }
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
        let mut open_settings = false;

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
                    ui.spacing_mut().slider_width = 92.0;
                    if ui
                        .add(egui::Slider::new(&mut gain, 0.0..=1.2).show_value(false))
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
                        .selectable_label(self.bottom == Bottom::Mix, "MIX")
                        .on_hover_text("mix rack: master + every track")
                        .clicked()
                    {
                        toggle_bottom = Some(Bottom::Mix);
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
                    if ui
                        .small_button("AUDIO")
                        .on_hover_text("output device, microphone, pack folders")
                        .clicked()
                    {
                        open_settings = true;
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
        if open_settings {
            self.refresh_devices();
            self.show_settings = true;
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
        let mut preview_src: Option<PathBuf> = None;
        let mut drag_src: Option<PathBuf> = None;
        let mut swap: Option<(usize, usize)> = None;
        let mut drop_pack: Option<usize> = None;

        // Keep the "add pack folder" footer on screen: the list scrolls inside
        // the space that is left, instead of eating the whole panel.
        let list_h = (ui.available_height() - 104.0).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(list_h)
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
                                    .sense(egui::Sense::click_and_drag()),
                                );
                                if resp.clicked() {
                                    preview_src = Some(sound.path.clone());
                                }
                                if resp.drag_started() {
                                    drag_src = Some(sound.path.clone());
                                }
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                }
                                resp.on_hover_text("click = preview · drag onto the timeline");
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
            if ui
                .button("BROWSE...")
                .on_hover_text("pick any folder of samples on this machine")
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose a sound pack folder")
                    .pick_folder()
                {
                    self.add_pack_or_file(dir);
                }
            }
            if ui.button("RESCAN packs/").clicked() {
                self.scan_packs_dir(PathBuf::from("packs"));
            }
        });

        if let Some(path) = preview_src {
            self.preview_sound(&path);
        }
        if let Some(path) = drag_src {
            if let Some(data) = self.cached_clip(&path) {
                let pos = ui
                    .ctx()
                    .input(|i| i.pointer.interact_pos().or_else(|| i.pointer.latest_pos()))
                    .unwrap_or(egui::Pos2::ZERO);
                self.sound_drag = Some(SoundDrag { data, pos });
            }
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

        let list_h = (ui.available_height() - 104.0).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(list_h)
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
            if ui
                .button("BROWSE...")
                .on_hover_text("pick your VST3 / CLAP / VST folder")
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose a plugin folder")
                    .pick_folder()
                {
                    self.scan_plugin_dir(dir);
                }
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
impl LosAngelesApp {
    // -- bottom panel: piano roll + effects ---------------------------------

    fn bottom_panel(&mut self, ctx: &egui::Context) {
        if self.bottom == Bottom::Closed {
            return;
        }
        let theme = self.theme.clone();
        let mut close = false;

        egui::TopBottomPanel::bottom("bottom")
            .resizable(true)
            .default_height(match self.bottom {
                Bottom::Piano => 240.0,
                Bottom::Mix => 340.0,
                _ => 190.0,
            })
            .min_height(120.0)
            .frame(
                egui::Frame::none()
                    .fill(theme.panel)
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
            )
            .show(ctx, |ui| {
                let sel = self.selected.min(self.tracks.len().saturating_sub(1));
                let title = self
                    .tracks
                    .get(sel)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();

                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(match self.bottom {
                            Bottom::Piano => "PIANO ROLL",
                            Bottom::Mix => "MIX RACK — MASTER + TRACKS",
                            _ => "TRACK FX / PLUGINS",
                        })
                        .size(10.0)
                        .strong()
                        .color(theme.accent),
                    );
                    ui.label(egui::RichText::new(title).size(10.0).color(theme.dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("x").clicked() {
                            close = true;
                        }
                    });
                });

                match self.bottom {
                    Bottom::Piano => self.piano_panel(ui, &theme),
                    Bottom::Mix => self.mix_rack_panel(ui, &theme),
                    _ => self.fx_panel(ui, &theme),
                }
            });

        if close {
            self.bottom = Bottom::Closed;
        }
    }

    fn piano_panel(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        if self.tracks.is_empty() {
            return;
        }
        let sel = self.selected.min(self.tracks.len() - 1);
        if self.roll_track != sel {
            self.roll_track = sel;
            self.roll.notes = self.tracks[sel].notes.clone();
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("snap").size(9.0).color(theme.dim));
            for (label, step) in [("1/32", 0.0625f32), ("1/16", 0.125), ("1/8", 0.25), ("1/4", 0.5)] {
                if ui
                    .selectable_label((self.roll.grid - step).abs() < 1e-4, label)
                    .clicked()
                {
                    self.roll.grid = step;
                }
            }
            if ui
                .selectable_label(self.roll.grid <= 0.0, "off")
                .clicked()
            {
                self.roll.grid = 0.0;
            }
            if ui.small_button("clear").clicked() {
                self.roll.notes.clear();
                self.roll.changed = true;
            }
            ui.label(
                egui::RichText::new("click adds · drag moves · right-click deletes · alt = free")
                    .size(9.0)
                    .color(theme.dim),
            );
        });

        self.roll.changed = false;
        let playhead = self.position_seconds();
        self.roll.show(ui, theme, playhead);
        if self.roll.changed {
            self.tracks[sel].notes = self.roll.notes.clone();
            self.sync_mix();
            self.dirty = true;
        }
    }

    /// Master + every track in one horizontal rack; insert slots are the hook
    /// for the plugin runtime that lands next.
    fn mix_rack_panel(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        let out = mixrack::show(
            ui,
            theme,
            &mut self.tracks,
            self.selected,
            &mut self.master,
            self.gain,
            self.meter,
            &self.plugins,
        );
        if let Some(i) = out.select {
            self.select_track(i);
        }
        if let Some(i) = out.open_fx {
            self.selected = i;
            self.bottom = Bottom::Fx;
        }
        if let Some(g) = out.gain {
            self.set_gain(g);
        }
        if out.changed {
            self.sync_mix();
            self.dirty = true;
        }
    }

    fn fx_panel(&mut self, ui: &mut egui::Ui, theme: &Theme) {
        if self.tracks.is_empty() {
            return;
        }
        let sel = self.selected.min(self.tracks.len() - 1);
        let mut changed = false;
        let mut remove_effect: Option<usize> = None;
        let mut remove_plugin: Option<usize> = None;
        let mut add_effect: Option<EffectKind> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for kind in EffectKind::ALL {
                        if ui.small_button(format!("+ {}", kind.label())).clicked() {
                            add_effect = Some(kind);
                        }
                    }
                });
                ui.add_space(4.0);

                let track = &mut self.tracks[sel];
                for (i, fx) in track.effects.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut fx.enabled, "").changed() {
                            changed = true;
                        }
                        ui.label(
                            egui::RichText::new(fx.kind.label())
                                .size(11.0)
                                .color(theme.accent),
                        );
                        ui.spacing_mut().slider_width = 130.0;
                        if ui
                            .add(egui::Slider::new(&mut fx.amount, 0.0..=1.0).show_value(false))
                            .changed()
                        {
                            changed = true;
                        }
                        ui.label(
                            egui::RichText::new(format!("{:.0}%", fx.amount * 100.0))
                                .size(9.0)
                                .color(theme.dim),
                        );
                        if ui.small_button("x").clicked() {
                            remove_effect = Some(i);
                        }
                    });
                }

                if !track.plugins.is_empty() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("PLUGIN CHAIN").size(9.0).color(theme.dim));
                }
                for (i, pl) in track.plugins.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut pl.enabled, "").changed() {
                            changed = true;
                        }
                        ui.label(
                            egui::RichText::new(pl.kind.label())
                                .size(9.0)
                                .monospace()
                                .color(theme.accent_hot),
                        );
                        ui.label(egui::RichText::new(&pl.name).size(11.0).color(theme.cream));
                        ui.spacing_mut().slider_width = 90.0;
                        if ui
                            .add(egui::Slider::new(&mut pl.wet, 0.0..=1.0).show_value(false))
                            .changed()
                        {
                            changed = true;
                        }
                        if ui.small_button("x").clicked() {
                            remove_plugin = Some(i);
                        }
                    });
                    ui.label(
                        egui::RichText::new(format!("   {}", pl.path.display()))
                            .size(8.0)
                            .color(theme.dim),
                    );
                }

                if track.effects.is_empty() && track.plugins.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "no effects yet — add one above, or pick a CLAP / VST3 / VST in the left browser",
                        )
                        .size(10.0)
                        .color(theme.dim),
                    );
                }
            });

        if let Some(kind) = add_effect {
            self.tracks[sel].effects.push(tracks::Effect::new(kind));
            changed = true;
        }
        if let Some(i) = remove_effect {
            self.tracks[sel].effects.remove(i);
            changed = true;
        }
        if let Some(i) = remove_plugin {
            self.tracks[sel].plugins.remove(i);
            changed = true;
        }
        if changed {
            self.sync_mix();
            self.dirty = true;
        }
    }
}
impl LosAngelesApp {
    // -- timeline ----------------------------------------------------------

    fn time_to_x(&self, rect: &egui::Rect, t: f32) -> f32 {
        rect.left() + (t - self.scroll) * self.zoom
    }

    fn x_to_time(&self, rect: &egui::Rect, x: f32) -> f32 {
        self.scroll + (x - rect.left()) / self.zoom.max(1.0)
    }

    fn track_at_y(&self, rect: &egui::Rect, y: f32) -> Option<usize> {
        let top = rect.top() + RULER_H;
        if y < top {
            return None;
        }
        let local = y - top + self.vscroll;
        let idx = (local / (TRACK_H + LANE_GAP)).floor() as usize;
        if idx < self.tracks.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn timeline(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        let mut drop_files: Option<(Vec<PathBuf>, usize, f32)> = None;

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme.bg))
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                let painter = ui.painter_at(rect);

                let head_w = 176.0_f32.min(rect.width() * 0.45);
                let lane_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + head_w, rect.top() + RULER_H),
                    rect.max,
                );
                self.lane_rect = lane_rect;

                // Scroll / zoom. Disabled while dragging a clip, otherwise the
                // timeline scrolls under the pointer instead of the clip
                // actually moving under it.
                if self.clip_drag.is_none() {
                    if let Some(hp) = resp.hover_pos() {
                        let (dy, dx, ctrl, shift) = ui.input(|i| {
                            (
                                i.smooth_scroll_delta.y,
                                i.smooth_scroll_delta.x,
                                i.modifiers.ctrl || i.modifiers.command,
                                i.modifiers.shift,
                            )
                        });
                        if ctrl && dy.abs() > 0.1 {
                            let anchor = self.x_to_time(&lane_rect, hp.x);
                            self.zoom = (self.zoom * (dy * 0.004).exp()).clamp(8.0, 4000.0);
                            self.scroll = anchor - (hp.x - lane_rect.left()) / self.zoom;
                        } else if shift && dy.abs() > 0.1 {
                            self.scroll -= dy / self.zoom;
                        } else {
                            if dy.abs() > 0.1 {
                                self.vscroll -= dy;
                            }
                            if dx.abs() > 0.1 {
                                self.scroll -= dx / self.zoom;
                            }
                        }
                    }
                }
                self.scroll = self.scroll.max(0.0);
                let max_scroll = (self.tracks.len() as f32 * (TRACK_H + LANE_GAP) - lane_rect.height())
                    .max(0.0);
                self.vscroll = self.vscroll.clamp(0.0, max_scroll);

                // Ruler + lane backgrounds.
                let ruler = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top()),
                    egui::pos2(rect.right(), rect.top() + RULER_H),
                );
                painter.rect_filled(ruler, egui::Rounding::ZERO, theme.panel);
                painter.line_segment(
                    [ruler.left_bottom(), ruler.right_bottom()],
                    egui::Stroke::new(1.0_f32, theme.grid),
                );

                let step = self.snap_step();
                let mut t = (self.scroll / step).floor() * step;
                while t <= self.scroll + lane_rect.width() / self.zoom {
                    if t >= 0.0 {
                        let x = self.time_to_x(&lane_rect, t);
                        if x >= lane_rect.left() {
                            painter.line_segment(
                                [
                                    egui::pos2(x, ruler.top() + 14.0),
                                    egui::pos2(x, ruler.bottom()),
                                ],
                                egui::Stroke::new(1.0_f32, theme.grid),
                            );
                            painter.text(
                                egui::pos2(x + 4.0, ruler.top() + 3.0),
                                egui::Align2::LEFT_TOP,
                                format!("{t:.0}s"),
                                egui::FontId::proportional(9.0),
                                theme.dim,
                            );
                        }
                    }
                    t += step;
                }

                // Tracks.
                let mut header_actions: Vec<(usize, HeaderAction)> = Vec::new();
                let mut clip_rects: Vec<(usize, egui::Rect)> = Vec::new();
                for i in 0..self.tracks.len() {
                    let top = RULER_H + i as f32 * (TRACK_H + LANE_GAP) - self.vscroll;
                    if top + TRACK_H < RULER_H || top > rect.height() {
                        continue;
                    }
                    let row = egui::Rect::from_min_max(
                        egui::pos2(rect.left(), rect.top() + top),
                        egui::pos2(rect.right(), rect.top() + top + TRACK_H),
                    );
                    let selected = i == self.selected;
                    let lane_fill = if i % 2 == 0 { theme.lane } else { theme.lane_alt };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(row.left() + head_w, row.top()),
                            row.max,
                        ),
                        egui::Rounding::ZERO,
                        lane_fill,
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            row.min,
                            egui::pos2(row.left() + head_w, row.bottom()),
                        ),
                        egui::Rounding::ZERO,
                        if selected { theme.panel_hi } else { theme.panel },
                    );
                    painter.line_segment(
                        [
                            egui::pos2(row.left() + head_w, row.top()),
                            egui::pos2(row.left() + head_w, row.bottom()),
                        ],
                        egui::Stroke::new(1.0_f32, theme.grid),
                    );

                    if let Some(action) = self.track_header(ui, &theme, i, row, head_w) {
                        header_actions.push((i, action));
                    }

                    // Clips.
                    for (ci, clip) in self.clips.iter().enumerate() {
                        if clip.track != i {
                            continue;
                        }
                        let x0 = self.time_to_x(&lane_rect, clip.start);
                        let x1 = self.time_to_x(&lane_rect, clip.start + clip.data.duration);
                        let visible = egui::Rect::from_min_max(
                            egui::pos2(x0.max(lane_rect.left()), row.top() + 8.0),
                            egui::pos2(x1.min(lane_rect.right()), row.bottom() - 8.0),
                        );
                        if visible.width() < 2.0 || visible.height() < 4.0 {
                            continue;
                        }
                        clip_rects.push((ci, visible));
                        let dragging = self.clip_drag.map(|d| d.idx == ci).unwrap_or(false);
                        let color = self.tracks[i].color;
                        painter.rect_filled(
                            visible,
                            egui::Rounding::same(3.0),
                            color.gamma_multiply(if dragging { 0.45 } else { 0.28 }),
                        );
                        painter.rect_stroke(
                            visible,
                            egui::Rounding::same(3.0),
                            egui::Stroke::new(1.0_f32, color.gamma_multiply(0.9)),
                        );
                        // Name plate.
                        painter.text(
                            visible.left_top() + egui::vec2(5.0, 4.0),
                            egui::Align2::LEFT_TOP,
                            &clip.data.name,
                            egui::FontId::proportional(10.0),
                            theme.cream,
                        );
                        // Waveform.
                        let peaks = &clip.data.peaks;
                        if !peaks.is_empty() {
                            let mid = visible.center().y + 4.0;
                            let half = visible.height() * 0.30;
                            let buckets = (visible.width() as usize).max(1);
                            for b in 0..buckets {
                                let p0 = b * peaks.len() / buckets;
                                let p1 = ((b + 1) * peaks.len() / buckets).max(p0 + 1);
                                let amp = peaks[p0.min(peaks.len() - 1)..p1.min(peaks.len())]
                                    .iter()
                                    .fold(0.0f32, |m, v| m.max(v.abs()));
                                let x = visible.left() + b as f32;
                                painter.line_segment(
                                    [
                                        egui::pos2(x, mid - amp * half),
                                        egui::pos2(x, mid + amp * half),
                                    ],
                                    egui::Stroke::new(1.0_f32, color.gamma_multiply(0.95)),
                                );
                            }
                        }
                    }
                }

                // Playhead.
                let px = self.time_to_x(&lane_rect, self.position_seconds());
                if px >= lane_rect.left() && px <= lane_rect.right() {
                    painter.line_segment(
                        [
                            egui::pos2(px, ruler.top()),
                            egui::pos2(px, rect.bottom()),
                        ],
                        egui::Stroke::new(1.4_f32, theme.accent),
                    );
                }

                // Clip interaction: one widget per visible clip. A press lands
                // on exactly the clip under the pointer, and while dragging the
                // row under the pointer decides the track — no panel-wide guess.
                for (ci, crect) in clip_rects.iter() {
                    let cresp = ui.interact(
                        *crect,
                        egui::Id::new(("clip", *ci)),
                        egui::Sense::click_and_drag(),
                    );
                    let pressed = cresp.drag_started_by(egui::PointerButton::Primary)
                        || cresp.clicked();
                    if pressed {
                        let track = self.clips[*ci].track;
                        self.select_track(track);
                    }
                    if cresp.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(pos) = cresp.interact_pointer_pos() {
                            let grabbed = self.x_to_time(&lane_rect, pos.x);
                            let clip = &self.clips[*ci];
                            let offset = (grabbed - clip.start)
                                .clamp(0.0, clip.data.duration.max(0.01));
                            self.clip_drag = Some(ClipDrag { idx: *ci, offset });
                        }
                    }
                }
                if let Some(drag) = self.clip_drag {
                    let pointer = ui.input(|i| {
                        i.pointer
                            .interact_pos()
                            .or_else(|| i.pointer.latest_pos())
                    });
                    if ui.input(|i| i.pointer.primary_down()) {
                        if let Some(pos) = pointer {
                            let time = (self.x_to_time(&lane_rect, pos.x) - drag.offset).max(0.0);
                            // Clamp into the lane area so a pointer slightly
                            // above/below the grid still hits a real track.
                            let y = pos.y.clamp(lane_rect.top() + 1.0, lane_rect.max.y - 1.0);
                            let track = self
                                .track_at_y(&rect, y)
                                .unwrap_or(self.clips[drag.idx].track);
                            if let Some(clip) = self.clips.get_mut(drag.idx) {
                                clip.start = time;
                                clip.track = track;
                                self.dirty = true;
                            }
                        }
                    } else {
                        // Button released: hand the new position to the mixer.
                        self.clip_drag = None;
                        self.sync_mix();
                    }
                } else if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        if lane_rect.contains(pos) {
                            if let Some(t) = self.track_at_y(&rect, pos.y) {
                                self.select_track(t);
                            }
                        }
                    }
                }

                // Files dropped from the OS land where the pointer is.
                let dropped: Vec<PathBuf> = ui.input(|i| {
                    i.raw
                        .dropped_files
                        .iter()
                        .filter_map(|f| f.path.clone())
                        .collect()
                });
                if !dropped.is_empty() {
                    let fallback = {
                        let t = self.position_seconds();
                        egui::pos2(self.time_to_x(&lane_rect, t), lane_rect.top() + 1.0)
                    };
                    let pos = ui
                        .input(|i| i.pointer.latest_pos().or_else(|| i.pointer.hover_pos()))
                        .unwrap_or(fallback);
                    let y = pos.y.clamp(lane_rect.top() + 1.0, lane_rect.max.y - 1.0);
                    let track = self.track_at_y(&rect, y).unwrap_or(self.selected);
                    let x = pos.x.clamp(lane_rect.left(), lane_rect.right());
                    let time = self.x_to_time(&lane_rect, x).max(0.0);
                    drop_files = Some((dropped, track, time));
                }

                if self.playing {
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                }

                for (i, action) in header_actions {
                    match action {
                        HeaderAction::Select => self.select_track(i),
                        HeaderAction::Remove => {
                            self.remove_track(i);
                            self.sync_mix();
                        }
                        HeaderAction::Mix => {
                            self.dirty = true;
                            self.sync_mix();
                        }
                        HeaderAction::OpenFx => {
                            self.selected = i;
                            self.bottom = Bottom::Fx;
                        }
                        HeaderAction::OpenPiano => {
                            self.selected = i;
                            self.roll_track = usize::MAX;
                            self.bottom = Bottom::Piano;
                        }
                    }
                }
            });

        if let Some((files, track, time)) = drop_files {
            self.add_files_at(files, track, time);
        }
    }

    fn track_header(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        i: usize,
        row: egui::Rect,
        head_w: f32,
    ) -> Option<HeaderAction> {
        let mut action = None;
        let inner = egui::Rect::from_min_max(
            egui::pos2(row.left() + 6.0, row.top() + 4.0),
            egui::pos2(row.left() + head_w - 6.0, row.bottom() - 4.0),
        );
        if inner.width() < 40.0 {
            return None;
        }

        let color = self.tracks[i].color;
        let mut ui = ui.child_ui(
            inner,
            egui::Layout::top_down(egui::Align::Min),
            None,
        );
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(&self.tracks[i].name).size(11.0).color(color),
                    )
                    .sense(egui::Sense::click()),
                )
                .clicked()
            {
                action = Some(HeaderAction::Select);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("x").on_hover_text("delete track").clicked() {
                    action = Some(HeaderAction::Remove);
                }
                if ui.small_button("fx").clicked() {
                    action = Some(HeaderAction::OpenFx);
                }
                let notes = self.tracks[i].notes.len();
                if ui
                    .small_button("pno")
                    .on_hover_text(format!("piano roll · {notes} note(s)"))
                    .clicked()
                {
                    action = Some(HeaderAction::OpenPiano);
                }
            });
        });

        let mut vol = self.tracks[i].volume;
        ui.spacing_mut().slider_width = (inner.width() - 64.0).max(40.0);
        let slider = egui::Slider::new(&mut vol, 0.0..=1.5).show_value(false);
        if ui.add(slider).changed() {
            self.tracks[i].volume = vol;
            action = Some(HeaderAction::Mix);
        }

        ui.horizontal(|ui| {
            let muted = self.tracks[i].muted;
            if ui.small_button(if muted { "M!" } else { "M " }).clicked() {
                self.tracks[i].muted = !muted;
                action = Some(HeaderAction::Mix);
            }
            let solo = self.tracks[i].solo;
            if ui.small_button(if solo { "S!" } else { "S " }).clicked() {
                self.tracks[i].solo = !solo;
                action = Some(HeaderAction::Mix);
            }
            let fx = self.tracks[i].effects.len();
            let plugs = self.tracks[i].plugins.len();
            ui.label(
                egui::RichText::new(format!("{fx} fx · {plugs} plug"))
                    .size(9.0)
                    .color(theme.dim),
            );
        });

        action
    }
}

#[derive(Clone, Copy)]
enum HeaderAction {
    Select,
    Remove,
    Mix,
    OpenFx,
    OpenPiano,
}

impl LosAngelesApp {
    // -- audio settings window --------------------------------------------

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let theme = self.theme.clone();
        let mut open = self.show_settings;
        let mut pick_output: Option<Option<String>> = None;
        let mut pick_input: Option<Option<String>> = None;
        let mut rescan = false;
        let mut test_tone = false;

        egui::Window::new("AUDIO")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(470.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("OUTPUT DEVICE")
                        .size(9.0)
                        .color(theme.dim),
                );
                let mut out_choice = self.output_choice.clone();
                egui::ComboBox::from_label("")
                    .selected_text(
                        self.output_choice
                            .clone()
                            .unwrap_or_else(|| "system default".to_string()),
                    )
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut out_choice, None, "system default");
                        for dev in &self.output_devices {
                            ui.selectable_value(&mut out_choice, Some(dev.clone()), dev);
                        }
                    });
                if out_choice != self.output_choice {
                    pick_output = Some(out_choice);
                }
                if self.output_devices.is_empty() {
                    ui.label(
                        egui::RichText::new("no output devices reported by the host")
                            .size(10.0)
                            .color(theme.red),
                    );
                }
                match &self.engine {
                    Some(engine) => {
                        let info = &engine.info;
                        ui.label(
                            egui::RichText::new(format!(
                                "running · {} · {} Hz · {} ch",
                                info.device, info.sample_rate, info.channels
                            ))
                            .size(10.0)
                            .color(theme.accent),
                        );
                    }
                    None => {
                        ui.label(
                            egui::RichText::new(match &self.engine_error {
                                Some(err) => format!("offline · {err}"),
                                None => "offline".to_string(),
                            })
                            .size(10.0)
                            .color(theme.red),
                        );
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("INPUT / MICROPHONE").size(9.0).color(theme.dim));
                let mut in_choice = self.input_choice.clone();
                egui::ComboBox::from_label("")
                    .selected_text(
                        self.input_choice
                            .clone()
                            .unwrap_or_else(|| "system default".to_string()),
                    )
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut in_choice, None, "system default");
                        for dev in &self.input_devices {
                            ui.selectable_value(&mut in_choice, Some(dev.clone()), dev);
                        }
                    });
                if in_choice != self.input_choice {
                    pick_input = Some(in_choice);
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("level").size(10.0).color(theme.dim));
                    ui.add(
                        egui::ProgressBar::new(self.input_peak.clamp(0.0, 1.0))
                            .desired_width(220.0)
                            .text(if self.input.is_some() { "live" } else { "off" }),
                    );
                });
                if let Some(err) = &self.input_error {
                    ui.label(egui::RichText::new(err).size(10.0).color(theme.red));
                }

                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("RESCAN DEVICES").clicked() {
                        rescan = true;
                    }
                    if ui
                        .button("TEST TONE")
                        .on_hover_text("toggle the 440 Hz reference tone")
                        .clicked()
                    {
                        test_tone = true;
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "Sample packs are added in the SOUNDS tab of the left browser (BROWSE... picks any folder).",
                    )
                    .size(9.0)
                    .color(theme.dim),
                );
            });

        self.show_settings = open;
        if rescan {
            self.refresh_devices();
        }
        if let Some(choice) = pick_output {
            self.set_output_device(choice);
        }
        if let Some(choice) = pick_input {
            self.set_input_device(choice);
        }
        if test_tone {
            self.set_tone(!self.tone);
        }
    }
}

impl eframe::App for LosAngelesApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let space = ctx.input(|i| i.key_pressed(egui::Key::Space));
        if space {
            self.set_playing(!self.playing);
        }
        self.poll_meter();
        self.poll_input();

        self.top_bar(ctx);
        self.status_bar(ctx);
        self.browser_panel(ctx);
        self.bottom_panel(ctx);
        self.timeline(ctx);
        self.settings_window(ctx);
        self.drag_and_drop_sound(ctx);

        if self.dirty {
            self.dirty = false;
        }
        if self.playing || self.meter > 0.01 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([940.0, 620.0])
            .with_title("LOSANGELES — grid-free DAW"),
        ..Default::default()
    };

    eframe::run_native(
        "LOSANGELES",
        options,
        Box::new(|cc| {
            Ok(Box::new(LosAngelesApp::new(cc)) as Box<dyn eframe::App>)
        }),
    )
}



