//! cpal audio engine and the lock-free UI -> audio snapshot protocol.
//!
//! The audio callback never allocates and never locks: the UI builds a
//! `MixSnapshot` (tracks, clips, notes) and pushes it through an `rtrb` ring
//! buffer. Everything the UI reads back flows through atomics.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::pianoroll::Note;
use crate::tracks::PlacedClip;

/// Fixed DSP ceiling on the audio thread (no allocation there).
pub const MAX_DSP_TRACKS: usize = 64;
const CMD_QUEUE: usize = 128;

#[derive(Clone, Copy)]
pub struct TrackMix {
    pub volume: f32,
    pub drive: f32,
    pub lowpass: f32,
    pub highpass: f32,
    pub tremolo: f32,
}

impl Default for TrackMix {
    fn default() -> Self {
        Self {
            volume: 1.0,
            drive: 0.0,
            lowpass: 0.0,
            highpass: 0.0,
            tremolo: 0.0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct PlacedNote {
    pub track: usize,
    pub note: Note,
}

#[derive(Clone, Default)]
pub struct MixSnapshot {
    pub tracks: Vec<TrackMix>,
    pub clips: Vec<PlacedClip>,
    pub notes: Vec<PlacedNote>,
}

/// Позиция превью-плейхеда (длится до следующего события).
#[derive(Clone, Copy, Default)]
pub struct Preview {
    pub time: f32,
}

pub enum Command {
    Play,
    Stop,
    SetGain(f32),
    SetTone(bool),
    SetMix(Box<MixSnapshot>),
    Preview(Option<f32>), // Some(t) = сыграть preview, None = stop preview
    SetPreview(Box<Preview>), // позиция preview в секундах
}

pub enum Command {
    Play,
    Stop,
    SetGain(f32),
    SetTone(bool),
    SetMix(Box<MixSnapshot>),
}

/// State shared between the audio callback and the UI.
#[derive(Default)]
pub struct Shared {
    pub gain: AtomicU32, // f32 bits
    pub playing: AtomicBool,
    pub tone: AtomicBool,
    /// Transport position in frames.
    pub track_frames: AtomicU64,
    /// Peak of the last block (f32 bits).
    pub peak: AtomicU32,
    /// Peak of the last input block (f32 bits) — microphone meter.
    pub input_peak: AtomicU32,
    pub stream_errors: AtomicU64,
}

impl Shared {
    pub fn store_f32(slot: &AtomicU32, v: f32) {
        slot.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn load_f32(slot: &AtomicU32) -> f32 {
        f32::from_bits(slot.load(Ordering::Relaxed))
    }
}

pub struct EngineInfo {
    pub host: String,
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: u32,
}

impl EngineInfo {
    pub fn latency_ms(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            (self.buffer_frames as f32 / self.sample_rate as f32) * 1000.0
        }
    }
}

pub struct AudioEngine {
    _stream: cpal::Stream,
    producer: rtrb::Producer<Command>,
    pub info: EngineInfo,
}

/// Names of the available output devices (sorted, de-duplicated).
pub fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names: Vec<String> = Vec::new();
    if let Ok(devices) = host.output_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                if !names.iter().any(|n| n == &name) {
                    names.push(name);
                }
            }
        }
    }
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    names
}

/// Names of the available input devices (microphones, interfaces).
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names: Vec<String> = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                if !names.iter().any(|n| n == &name) {
                    names.push(name);
                }
            }
        }
    }
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    names
}

impl AudioEngine {
    pub fn start(shared: Arc<Shared>) -> Result<Self> {
        Self::start_on(shared, None)
    }

    /// Open the named output device, or the system default when `wanted` is `None`.
    pub fn start_on(shared: Arc<Shared>, wanted: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let host_name = host.id().name().to_string();

        let device = match wanted {
            Some(name) => host
                .output_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.name().ok().as_deref() == Some(name)))
                .ok_or_else(|| anyhow!("output device `{name}` not found"))?,
            None => host
                .default_output_device()
                .ok_or_else(|| anyhow!("no default output device found"))?,
        };
        let device_name = device.name().unwrap_or_else(|_| "unknown output".to_string());

        let supported = device
            .default_output_config()
            .map_err(|e| anyhow!("default output config unavailable: {e}"))?;
        let sample_format = supported.sample_format();
        let base_config: cpal::StreamConfig = supported.into();

        let mut last_err: Option<String> = None;

        // Try a small fixed buffer first (low latency), then fall back.
        for requested in [Some(256u32), Some(512), None] {
            let mut config = base_config.clone();
            if let Some(n) = requested {
                config.buffer_size = cpal::BufferSize::Fixed(n);
            }

            let (producer, consumer) = rtrb::RingBuffer::<Command>::new(CMD_QUEUE);

            let built = match sample_format {
                cpal::SampleFormat::F32 => {
                    build_stream::<f32>(&device, &config, shared.clone(), consumer)
                }
                cpal::SampleFormat::I16 => {
                    build_stream::<i16>(&device, &config, shared.clone(), consumer)
                }
                cpal::SampleFormat::U16 => {
                    build_stream::<u16>(&device, &config, shared.clone(), consumer)
                }
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

    pub fn send(&mut self, cmd: Command) {
        // Never blocks; a full queue drops the command (the UI resends state).
        let _ = self.producer.push(cmd);
    }
}

/// Microphone / line input: opens an input stream and publishes its peak.
pub struct InputEngine {
    _stream: cpal::Stream,
    pub device: String,
    pub channels: u16,
}

impl InputEngine {
    pub fn start(shared: Arc<Shared>, wanted: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();

        let device = match wanted {
            Some(name) => host
                .input_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.name().ok().as_deref() == Some(name)))
                .ok_or_else(|| anyhow!("input device `{name}` not found"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default input device found"))?,
        };
        let device_name = device.name().unwrap_or_else(|_| "unknown input".to_string());

        let supported = device
            .default_input_config()
            .map_err(|e| anyhow!("default input config unavailable: {e}"))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels;

        let stream = match sample_format {
            cpal::SampleFormat::F32 => build_input::<f32, _>(&device, &config, shared, |s| *s),
            cpal::SampleFormat::I16 => {
                build_input::<i16, _>(&device, &config, shared, |s| *s as f32 / 32_768.0)
            }
            cpal::SampleFormat::U16 => build_input::<u16, _>(&device, &config, shared, |s| {
                (*s as f32 - 32_768.0) / 32_768.0
            }),
            other => return Err(anyhow!("unsupported input sample format: {other:?}")),
        }
        .map_err(|e| anyhow!("cannot open `{device_name}`: {e}"))?;

        stream
            .play()
            .map_err(|e| anyhow!("input stream.play() failed: {e}"))?;

        Ok(Self {
            _stream: stream,
            device: device_name,
            channels,
        })
    }
}

fn build_input<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
    to_f32: F,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    F: Fn(&T) -> f32 + Send + 'static,
{
    let err_shared = shared.clone();
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            let mut peak = 0.0f32;
            for s in data {
                let a = to_f32(s).abs();
                if a > peak {
                    peak = a;
                }
            }
            // Fall back gently so a silent mic does not freeze the meter.
            let prev = Shared::load_f32(&shared.input_peak);
            Shared::store_f32(&shared.input_peak, peak.max(prev * 0.55));
        },
        move |_err| {
            err_shared.stream_errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )
}

/// Per-track filter memory, living on the audio thread.
#[derive(Clone, Copy, Default)]
struct TrackDsp {
    lp: f32,
    hp: f32,
    gain: f32,
    trem_phase: f32,
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
    let tone_step = 440.0 / sample_rate;

    let mut gain_current = 0.0f32;
    let mut gain_target = Shared::load_f32(&shared.gain);
    let mut playing = false;
    let mut tone = shared.tone.load(Ordering::Relaxed);
    let mut phase = 0.0f32;
    let mut frames = 0u64;
    let mut mix = MixSnapshot::default();
    let mut dsp = [TrackDsp::default(); MAX_DSP_TRACKS];

    let err_shared = shared.clone();

    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::Play => playing = true,
                    Command::Stop => {
                        playing = false;
                        frames = 0;
                    }
                    Command::SetGain(g) => gain_target = g.clamp(0.0, 2.0),
                    Command::SetTone(on) => tone = on,
                    Command::SetMix(snapshot) => mix = *snapshot,
                }
            }

            let mut peak = 0.0f32;

            for frame in data.chunks_mut(channels) {
                // One-pole smoothing keeps gain changes click-free.
                gain_current += (gain_target - gain_current) * 0.0015;

                let mut sample = 0.0f32;
                if playing {
                    let t = frames as f32 / sample_rate;

                    if tone {
                        phase += tone_step;
                        if phase >= 1.0 {
                            phase -= 1.0;
                        }
                        sample += (phase * std::f32::consts::TAU).sin() * 0.125;
                    }

                    for (ti, tm) in mix.tracks.iter().enumerate() {
                        if ti >= MAX_DSP_TRACKS {
                            break;
                        }
                        let mut ts = 0.0f32;

                        for clip in mix.clips.iter() {
                            if clip.track != ti {
                                continue;
                            }
                            let local = t - clip.start;
                            if local < 0.0 || local >= clip.data.duration {
                                continue;
                            }
                            let src_rate = clip.data.sample_rate.max(1) as f32;
                            let pos = local * src_rate;
                            let i = pos as usize;
                            if i + 1 >= clip.data.samples.len() {
                                continue;
                            }
                            let frac = pos - i as f32;
                            let s0 = clip.data.samples[i];
                            let s1 = clip.data.samples[i + 1];
                            ts += (s0 + (s1 - s0) * frac) * 0.9;
                        }

                        for pn in mix.notes.iter() {
                            if pn.track == ti {
                                ts += note_sample(&pn.note, t - pn.note.start);
                            }
                        }

                        let d = &mut dsp[ti];

                        if tm.drive > 0.0 {
                            let g = 1.0 + tm.drive * 6.0;
                            ts = (ts * g).tanh() / (1.0 + tm.drive * 2.0);
                        }
                        if tm.lowpass > 0.0 {
                            let coef = 0.35 * (1.0 - tm.lowpass) + 0.004;
                            d.lp += coef * (ts - d.lp);
                            ts = d.lp;
                        }
                        if tm.highpass > 0.0 {
                            let coef = 0.25 * (1.0 - tm.highpass) + 0.004;
                            d.hp += coef * (ts - d.hp);
                            ts -= d.hp;
                        }
                        if tm.tremolo > 0.0 {
                            d.trem_phase += 5.0 / sample_rate;
                            if d.trem_phase >= 1.0 {
                                d.trem_phase -= 1.0;
                            }
                            let lfo = 0.5 - 0.5 * (d.trem_phase * std::f32::consts::TAU).cos();
                            ts *= 1.0 - tm.tremolo * (1.0 - lfo);
                        }

                        d.gain += (tm.volume - d.gain) * 0.0015;
                        sample += ts * d.gain;
                    }
                }

                let out = (sample * gain_current).clamp(-1.0, 1.0);
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
            err_shared.stream_errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )
}

/// Stateless note voice: sine + octave with a short attack/release envelope.
pub fn note_sample(n: &Note, local: f32) -> f32 {
    if local < 0.0 || local >= n.len {
        return 0.0;
    }
    let freq = 440.0 * 2f32.powf((n.pitch as f32 - 69.0) / 12.0);
    let attack = 0.004f32;
    let release = 0.03f32;
    let env = (local / attack).min(1.0) * ((n.len - local) / release).min(1.0);
    let x = local * freq * std::f32::consts::TAU;
    (x.sin() * 0.6 + (x * 2.0).sin() * 0.15) * env * n.vel * 0.22
}
