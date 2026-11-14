//! Audio file decoding (wav / mp3 / ogg / flac) and waveform peak extraction.
//!
//! Stage 2: a file is decoded fully into memory as a mono mixdown. The audio
//! callback only reads that immutable buffer through an `Arc`, so the real-time
//! thread never allocates sample data or locks a file handle.

use std::fs::File;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Number of buckets in the pre-computed waveform (min/max resolution).
const PEAK_BUCKETS: usize = 4096;

pub struct ClipData {
    pub name: String,
    /// Mono mixdown at `sample_rate`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub duration: f32,
    /// Max |sample| per bucket, normalised to 0..1.
    pub peaks: Vec<f32>,
}

impl ClipData {
    pub fn decode(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(|e| anyhow!("unsupported format ({e})"))?;

        let mut format = probed.format;

        let (track_id, params) = {
            let track = format
                .tracks()
                .iter()
                .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                .ok_or_else(|| anyhow!("no decodable audio track"))?;
            (track.id, track.codec_params.clone())
        };

        let mut decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| anyhow!("unsupported codec ({e})"))?;

        let mut sample_rate = params.sample_rate.unwrap_or(48_000).max(1);
        let mut channels = params.channels.map(|c| c.count()).unwrap_or(2).max(1);
        let mut samples: Vec<f32> = Vec::new();
        let mut sample_buf: Option<SampleBuffer<f32>> = None;

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break
                }
                Err(SymphoniaError::ResetRequired) => break,
                Err(e) => return Err(anyhow!("read error ({e})")),
            };
            if packet.track_id() != track_id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    if sample_buf.is_none() {
                        let spec = *decoded.spec();
                        sample_rate = spec.rate.max(1);
                        channels = spec.channels.count().max(1);
                        let capacity = decoded.capacity().max(1);
                        sample_buf = Some(SampleBuffer::<f32>::new(capacity as u64, spec));
                    }
                    if let Some(buf) = sample_buf.as_mut() {
                        buf.copy_interleaved_ref(decoded);
                        let ch = channels.max(1);
                        for frame in buf.samples().chunks(ch) {
                            let mut sum = 0.0f32;
                            for s in frame {
                                sum += *s;
                            }
                            samples.push(sum / ch as f32);
                        }
                    }
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break
                }
                Err(e) => return Err(anyhow!("decode error ({e})")),
            }
        }

        if samples.is_empty() {
            return Err(anyhow!("file contains no audio frames"));
        }

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio")
            .to_string();
        let duration = samples.len() as f32 / sample_rate as f32;
        let peaks = build_peaks(&samples);

        Ok(Self {
            name,
            samples,
            sample_rate,
            duration,
            peaks,
        })
    }
}

fn build_peaks(samples: &[f32]) -> Vec<f32> {
    let buckets = PEAK_BUCKETS.min(samples.len()).max(1);
    let per = (samples.len() as f32 / buckets as f32).max(1.0);
    let mut peaks = Vec::with_capacity(buckets);
    let mut max = 0.0f32;

    for b in 0..buckets {
        let start = (b as f32 * per) as usize;
        let end = (((b + 1) as f32 * per) as usize).min(samples.len());
        let mut peak = 0.0f32;
        for s in &samples[start.min(samples.len())..end] {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
        }
        max = max.max(peak);
        peaks.push(peak);
    }

    if max > 0.0 {
        for p in peaks.iter_mut() {
            *p /= max;
        }
    }
    peaks
}
