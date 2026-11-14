//! LOSANGELES — experimental grid-free DAW.
//!
//! Stage 1 skeleton: application window, lock-free audio engine, empty free timeline.
//!
//! Planned module layout (grown step by step):
//!   src/audio/      — cpal engine, mixer graph, real-time safe plumbing
//!   src/timeline/   — grid-free timeline model (clips, seconds/ms, no BPM)
//!   src/ui/         — egui theme, timeline painter, panels
//!   src/constraints — creative constraints engine (no-undo, 3-track limit, ...)
//!   src/plugins/    — CLAP / VST3 hosting

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;

// ---------------------------------------------------------------------------
// Palette: warm, analog, slightly dusty
// ---------------------------------------------------------------------------

const COL_BG: egui::Color32 = egui::Color32::from_rgb(0x14, 0x11, 0x0F);
const COL_PANEL: egui::Color32 = egui::Color32::from_rgb(0x1B, 0x17, 0x14);
const COL_PANEL_HI: egui::Color32 = egui::Color32::from_rgb(0x24, 0x1E, 0x19);
const COL_LANE: egui::Color32 = egui::Color32::from_rgb(0x1F, 0x1A, 0x16);
const COL_LANE_ALT: egui::Color32 = egui::Color32::from_rgb(0x1C, 0x17, 0x14);
const COL_GRID: egui::Color32 = egui::Color32::from_rgb(0x31, 0x29, 0x24);
const COL_AMBER: egui::Color32 = egui::Color32::from_rgb(0xE0, 0xA1, 0x5C);
const COL_EMBER: egui::Color32 = egui::Color32::from_rgb(0xC9, 0x7B, 0x3A);
const COL_CREAM: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xDC, 0xC8);
const COL_DIM: egui::Color32 = egui::Color32::from_rgb(0x7A, 0x6A, 0x5C);
const COL_RED: egui::Color32 = egui::Color32::from_rgb(0xD4, 0x5A, 0x3C);

// ---------------------------------------------------------------------------
// UI -> audio thread commands (lock-free ring buffer, no allocation)
// ---------------------------------------------------------------------------

const CMD_QUEUE: usize = 128;

#[derive(Clone, Copy, Debug)]
enum Command {
    Play,
    Stop,
    SetGain(f32),
    SetTone(bool),
}

/// State shared between the audio callback and the UI.
///
/// Only atomics: the audio thread never blocks, never allocates, never locks.
#[derive(Default)]
struct Shared {
    gain: AtomicU32,        // f32 bits
    playing: AtomicBool,
    tone: AtomicBool,
    track_frames: AtomicU64, // transport position in frames
    peak: AtomicU32,         // f32 bits, last block peak
    stream_errors: AtomicU64,
}

impl Shared {
    fn store_f32(slot: &AtomicU32, v: f32) {
        slot.store(v.to_bits(), Ordering::Relaxed);
    }

    fn load_f32(slot: &AtomicU32) -> f32 {
        f32::from_bits(slot.load(Ordering::Relaxed))
    }
}

// ---------------------------------------------------------------------------
// Audio engine
// ---------------------------------------------------------------------------

struct EngineInfo {
    host: String,
    device: String,
    sample_rate: u32,
    channels: u16,
    buffer_frames: u32,
}

impl EngineInfo {
    fn latency_ms(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            (self.buffer_frames as f32 / self.sample_rate as f32) * 1000.0
        }
    }
}

struct AudioEngine {
    _stream: cpal::Stream,
    producer: rtrb::Producer<Command>,
    info: EngineInfo,
}

impl AudioEngine {
    fn start(shared: Arc<Shared>) -> Result<Self> {
        let host = cpal::default_host();
        let host_name = host.id().name().to_string();

        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device found"))?;
        let device_name = device.name().unwrap_or_else(|_| "unknown output".to_string());

        let supported = device
            .default_output_config()
            .map_err(|e| anyhow!("default output config unavailable: {e}"))?;
        let sample_format = supported.sample_format();
        let base_config: cpal::StreamConfig = supported.into();

        let mut last_err: Option<String> = None;

        // Try a small fixed buffer for low latency, then fall back.
        for requested in [Some(256u32), Some(512), None] {
            let mut config = base_config.clone();
            if let Some(n) = requested {
                config.buffer_size = cpal::BufferSize::Fixed(n);
            }

            let (producer, consumer) = rtrb::RingBuffer::<Command>::new(CMD_QUEUE);

            let built = match sample_format {
                cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, shared.clone(), consumer),
                cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, shared.clone(), consumer),
                cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, shared.clone(), consumer),
                other => {
                    last_err = Some(format!("unsupported sample format: {other:?}"));
                    continue;
                }
            };

            match built {
                Ok(stream) => {
                    if let Err(e) = stream.play() {
                        last_err = Some(format!("stream.play() failed: {e}"));
                        continue;
                    }
                    let buffer_frames = match config.buffer_size {
                        cpal::BufferSize::Fixed(n) => n,
                        cpal::BufferSize::Default => 0,
                    };
                    let info = EngineInfo {
                        host: host_name.clone(),
                        device: device_name.clone(),
                        sample_rate: config.sample_rate.0,
                        channels: config.channels,
                        buffer_frames,
                    };
                    return Ok(Self {
                        _stream: stream,
                        producer,
                        info,
                    });
                }
                Err(e) => last_err = Some(format!("{e}")),
            }
        }

        Err(anyhow!(
            last_err.unwrap_or_else(|| "could not open an output stream".into())
        ))
    }

    fn send(&mut self, cmd: Command) {
        // Never blocks; a full queue simply drops the command (UI resends state).
        let _ = self.producer.push(cmd);
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
    mut consumer: rtrb::Consumer<Command>,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let sample_rate = config.sample_rate.0.max(1) as f32;
    let tone_step = 440.0 / sample_rate; // 440 Hz reference tone

    let mut gain_current = 0.0f32;
    let mut gain_target = Shared::load_f32(&shared.gain);
    let mut playing = false;
    let mut tone = shared.tone.load(Ordering::Relaxed);
    let mut phase = 0.0f32;
    let mut frames = 0u64;

    let err_shared = shared.clone();

    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
            // Drain commands (bounded loop: at most the queue capacity).
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::Play => playing = true,
                    Command::Stop => {
                        playing = false;
                        frames = 0;
                    }
                    Command::SetGain(g) => gain_target = g.clamp(0.0, 2.0),
                    Command::SetTone(on) => tone = on,
                }
            }

            let mut peak = 0.0f32;

            for frame in data.chunks_mut(channels) {
                // One-pole smoothing keeps gain changes click-free.
                gain_current += (gain_target - gain_current) * 0.0015;

                let mut sample = 0.0f32;
                if playing && tone {
                    phase += tone_step;
                    if phase >= 1.0 {
                        phase -= 1.0;
                    }
                    sample = (phase * std::f32::consts::TAU).sin() * 0.125;
                }

                let out = sample * gain_current;
                let abs = out.abs();
                if abs > peak {
                    peak = abs;
                }

                if playing {
                    frames = frames.wrapping_add(1);
                }

                for ch in frame.iter_mut() {
                    *ch = T::from_sample(out);
                }
            }

            Shared::store_f32(&shared.peak, peak);
            shared.track_frames.store(frames, Ordering::Relaxed);
            shared.playing.store(playing, Ordering::Relaxed);
        },
        move |_err| {
            // No allocation / no formatting on the audio thread's error path.
            err_shared.stream_errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

const TRACK_COUNT: usize = 4;

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
}

impl LosAngelesApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_theme(&cc.egui_ctx);

        let shared = Arc::new(Shared::default());
        Shared::store_f32(&shared.gain, 0.8);

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
        };
        app.init_audio();
        app
    }

    fn init_audio(&mut self) {
        match AudioEngine::start(self.shared.clone()) {
            Ok(mut engine) => {
                engine.send(Command::SetGain(self.gain));
                engine.send(Command::SetTone(self.tone));
                self.engine = Some(engine);
                self.engine_error = None;
            }
            Err(e) => {
                self.engine = None;
                self.engine_error = Some(e.to_string());
            }
        }
    }

    fn set_playing(&mut self, on: bool) {
        if self.engine.is_none() {
            return;
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

    // -- panels ------------------------------------------------------------

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("transport")
            .exact_height(60.0)
            .frame(
                egui::Frame::none()
                    .fill(COL_PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("LOSANGELES")
                                .size(19.0)
                                .strong()
                                .color(COL_AMBER),
                        );
                        ui.label(
                            egui::RichText::new("grid-free  ·  off-grid groove")
                                .size(10.0)
                                .color(COL_DIM),
                        );
                    });

                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(10.0);

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

                    ui.add_space(8.0);
                    let tone_resp = ui.selectable_label(
                        self.tone,
                        egui::RichText::new("TONE 440").size(11.0).color(if self.tone {
                            COL_AMBER
                        } else {
                            COL_DIM
                        }),
                    );
                    if tone_resp.clicked() {
                        self.set_tone(!self.tone);
                    }
                    tone_resp.on_hover_text("Reference tone — press PLAY to verify audio output");

                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("MASTER").size(10.0).color(COL_DIM));
                    let mut gain = self.gain;
                    if ui
                        .add(egui::Slider::new(&mut gain, 0.0..=1.2).show_value(false))
                        .changed()
                    {
                        self.set_gain(gain);
                    }

                    // Peak meter
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(70.0, 8.0),
                        egui::Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(rect, egui::Rounding::same(2.0), COL_PANEL_HI);
                    let width = (rect.width() * self.meter.clamp(0.0, 1.0)).max(0.0);
                    if width > 0.0 {
                        let bar = egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(width, rect.height()),
                        );
                        let color = if self.meter > 0.9 { COL_RED } else { COL_EMBER };
                        painter.rect_filled(bar, egui::Rounding::same(2.0), color);
                    }

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let pos = self.position_seconds();
                            let mins = (pos / 60.0).floor() as u32;
                            let secs = pos - mins as f32 * 60.0;
                            ui.label(
                                egui::RichText::new(format!("{mins:02}:{secs:05.2}"))
                                    .monospace()
                                    .size(16.0)
                                    .color(COL_CREAM),
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("NO GRID")
                                    .size(10.0)
                                    .color(COL_EMBER)
                                    .strong(),
                            );
                        },
                    );
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(
                egui::Frame::none()
                    .fill(COL_PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    match &self.engine {
                        Some(engine) => {
                            let info = &engine.info;
                            ui.label(
                                egui::RichText::new("● AUDIO")
                                    .size(10.0)
                                    .color(COL_AMBER),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {} · {} Hz · {} ch · {} frames · {:.1} ms",
                                    info.host,
                                    info.device,
                                    info.sample_rate,
                                    info.channels,
                                    if info.buffer_frames == 0 {
                                        "default".to_string()
                                    } else {
                                        info.buffer_frames.to_string()
                                    },
                                    info.latency_ms(),
                                ))
                                .size(10.0)
                                .color(COL_DIM),
                            );
                            let errs = self.shared.stream_errors.load(Ordering::Relaxed);
                            if errs > 0 {
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(format!("stream errors: {errs}"))
                                        .size(10.0)
                                        .color(COL_RED),
                                );
                            }
                        }
                        None => {
                            ui.label(
                                egui::RichText::new("● AUDIO OFFLINE")
                                    .size(10.0)
                                    .color(COL_RED),
                            );
                            if let Some(err) = &self.engine_error {
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(err).size(10.0).color(COL_DIM),
                                );
                            }
                            ui.add_space(8.0);
                            if ui.small_button("RETRY").clicked() {
                                self.init_audio();
                            }
                        }
                    }

                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new("stage 1 · skeleton")
                                    .size(10.0)
                                    .color(COL_DIM),
                            );
                        },
                    );
                });
            });
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(COL_BG))
            .show(ctx, |ui| {
                self.timeline_canvas(ui);
            });
    }

    fn timeline_canvas(&mut self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;
        painter.rect_filled(rect, egui::Rounding::ZERO, COL_BG);

        const RULER_H: f32 = 26.0;
        const TRACK_H: f32 = 92.0;
        const GAP: f32 = 6.0;

        // -- zoom / pan ----------------------------------------------------
        if let Some(hover) = response.hover_pos() {
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_delta.abs() > 0.1 {
                let before = self.scroll + (hover.x - rect.left()) / self.zoom;
                let factor = (scroll_delta * 0.004).exp();
                self.zoom = (self.zoom * factor).clamp(4.0, 4000.0);
                self.scroll = before - (hover.x - rect.left()) / self.zoom;
            }
        }
        if response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary)
                && ui.input(|i| i.modifiers.alt))
        {
            self.scroll -= response.drag_delta().x / self.zoom;
        }
        self.scroll = self.scroll.max(0.0);

        // -- ruler ---------------------------------------------------------
        let ruler = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.right(), rect.top() + RULER_H),
        );
        painter.rect_filled(ruler, egui::Rounding::ZERO, COL_PANEL);

        let step = tick_step(self.zoom);
        let first = (self.scroll / step).floor() * step;
        let last_time = self.scroll + rect.width() / self.zoom;
        let mut t = first;
        while t <= last_time + step {
            if t >= 0.0 {
                let x = rect.left() + (t - self.scroll) * self.zoom;
                painter.line_segment(
                    [egui::pos2(x, ruler.bottom() - 8.0), egui::pos2(x, ruler.bottom())],
                    egui::Stroke::new(1.0, COL_DIM),
                );
                let label = if step < 1.0 {
                    format!("{t:.2}s")
                } else {
                    let secs = t as i64;
                    format!("{}:{:02}.{:01}", secs / 60, secs % 60, ((t - secs as f32) * 10.0) as i32)
                };
                painter.text(
                    egui::pos2(x + 4.0, ruler.top() + 4.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::monospace(10.0),
                    COL_DIM,
                );
            }
            t += step;
        }

        // -- tracks --------------------------------------------------------
        for i in 0..TRACK_COUNT {
            let top = rect.top() + RULER_H + 8.0 + i as f32 * (TRACK_H + GAP);
            if top > rect.bottom() {
                break;
            }
            let lane = egui::Rect::from_min_size(
                egui::pos2(rect.left(), top),
                egui::vec2(rect.width(), TRACK_H.min(rect.bottom() - top)),
            );
            painter.rect_filled(
                lane,
                egui::Rounding::same(3.0),
                if i % 2 == 0 { COL_LANE } else { COL_LANE_ALT },
            );
            painter.rect_stroke(
                lane,
                egui::Rounding::same(3.0),
                egui::Stroke::new(1.0, COL_GRID),
            );

            // second lines, just for orientation (no musical grid on purpose)
            painter.text(
                egui::pos2(lane.left() + 10.0, lane.top() + 6.0),
                egui::Align2::LEFT_TOP,
                format!("TRACK {:02}", i + 1),
                egui::FontId::proportional(11.0),
                COL_DIM,
            );
            painter.text(
                lane.center(),
                egui::Align2::CENTER_CENTER,
                "drag audio files here  (stage 2)",
                egui::FontId::proportional(12.0),
                COL_GRID,
            );
            painter.line_segment(
                [
                    egui::pos2(lane.left(), lane.center().y),
                    egui::pos2(lane.right(), lane.center().y),
                ],
                egui::Stroke::new(1.0, COL_GRID),
            );
        }

        // -- playhead ------------------------------------------------------
        let pos = self.position_seconds();
        let x = rect.left() + (pos - self.scroll) * self.zoom;
        if x >= rect.left() && x <= rect.right() {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.5, COL_AMBER),
            );
            painter.circle_filled(egui::pos2(x, rect.top() + RULER_H), 3.0, COL_AMBER);
        }

        // -- footer hint ---------------------------------------------------
        painter.text(
            egui::pos2(rect.left() + 12.0, rect.bottom() - 16.0),
            egui::Align2::LEFT_BOTTOM,
            format!(
                "zoom {:.0} px/s   ·   scroll {:.2}s   ·   wheel = zoom, alt+drag / middle-drag = pan   ·   space = play",
                self.zoom, self.scroll
            ),
            egui::FontId::proportional(10.0),
            COL_DIM,
        );
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.set_playing(!self.playing);
        }
    }
}

impl eframe::App for LosAngelesApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_meter();
        self.shortcuts(ctx);
        self.top_bar(ctx);
        self.status_bar(ctx);
        self.timeline_panel(ctx);
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

/// Choose a "round" tick spacing so labels stay ~90 px apart.
fn tick_step(zoom: f32) -> f32 {
    const STEPS: [f32; 12] = [
        0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0,
    ];
    let target = 90.0 / zoom.max(0.001);
    for s in STEPS {
        if s >= target {
            return s;
        }
    }
    120.0
}

fn install_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    visuals.override_text_color = Some(COL_CREAM);
    visuals.panel_fill = COL_PANEL;
    visuals.window_fill = COL_PANEL;
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x0F, 0x0C, 0x0A);
    visuals.faint_bg_color = COL_LANE;
    visuals.hyperlink_color = COL_AMBER;
    visuals.selection.bg_fill = COL_EMBER;
    visuals.selection.stroke = egui::Stroke::new(1.0, COL_CREAM);

    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = COL_PANEL;
    w.noninteractive.weak_bg_fill = COL_PANEL;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0, COL_GRID);
    w.noninteractive.fg_stroke = egui::Stroke::new(1.0, COL_CREAM);
    w.noninteractive.rounding = egui::Rounding::same(2.0);

    w.inactive.bg_fill = COL_PANEL_HI;
    w.inactive.weak_bg_fill = COL_PANEL_HI;
    w.inactive.bg_stroke = egui::Stroke::new(1.0, COL_GRID);
    w.inactive.fg_stroke = egui::Stroke::new(1.0, COL_CREAM);
    w.inactive.rounding = egui::Rounding::same(2.0);

    w.hovered.bg_fill = egui::Color32::from_rgb(0x33, 0x2A, 0x22);
    w.hovered.weak_bg_fill = egui::Color32::from_rgb(0x33, 0x2A, 0x22);
    w.hovered.bg_stroke = egui::Stroke::new(1.0, COL_EMBER);
    w.hovered.fg_stroke = egui::Stroke::new(1.0, COL_AMBER);
    w.hovered.rounding = egui::Rounding::same(2.0);

    w.active.bg_fill = egui::Color32::from_rgb(0x4A, 0x33, 0x1E);
    w.active.weak_bg_fill = egui::Color32::from_rgb(0x4A, 0x33, 0x1E);
    w.active.bg_stroke = egui::Stroke::new(1.0, COL_AMBER);
    w.active.fg_stroke = egui::Stroke::new(1.0, COL_CREAM);
    w.active.rounding = egui::Rounding::same(2.0);

    w.open.bg_fill = COL_PANEL_HI;
    w.open.weak_bg_fill = COL_PANEL_HI;
    w.open.bg_stroke = egui::Stroke::new(1.0, COL_GRID);
    w.open.fg_stroke = egui::Stroke::new(1.0, COL_CREAM);
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_title("LOSANGELES"),
        ..Default::default()
    };

    eframe::run_native(
        "LOSANGELES",
        options,
        Box::new(|cc| Ok(Box::new(LosAngelesApp::new(cc)))),
    )
}
