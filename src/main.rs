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
// __PART_B__
