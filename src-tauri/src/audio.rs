//! Continuous microphone capture into a ring buffer.
//!
//! The stream runs for the life of the process rather than starting on keypress.
//! Two reasons: opening a WASAPI stream costs 50-200ms, which would clip the start
//! of every utterance; and an always-full ring lets us reach *backwards* for pre-roll,
//! catching the word you started saying as you pressed the key.

use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

/// One energy band per ribbon, so the visualiser tracks the shape of the voice
/// rather than just its loudness.
pub const BANDS: usize = 7;
const FFT_SIZE: usize = 2048;
/// Speech range: fundamentals at the bottom, formants through the middle,
/// sibilance at the top.
const F_LOW: f32 = 80.0;
const F_HIGH: f32 = 8_000.0;

/// Whisper is trained on 16kHz mono.
pub const TARGET_RATE: u32 = 16_000;
/// How far back to reach when the hotkey goes down.
pub const PREROLL_MS: u32 = 200;
/// Ring capacity. Longer than any sane single dictation.
const RING_SECS: usize = 120;

/// Mono sample ring with absolute indexing, so a mark taken at press time stays
/// valid even after the ring has rotated underneath it.
struct Ring {
    buf: VecDeque<f32>,
    written: u64,
    cap: usize,
}

impl Ring {
    fn push(&mut self, s: f32) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(s);
        self.written += 1;
    }

    /// Absolute index of the oldest sample still held.
    fn base(&self) -> u64 {
        self.written - self.buf.len() as u64
    }

    fn since(&self, abs: u64) -> Vec<f32> {
        let off = abs.saturating_sub(self.base()) as usize;
        self.buf.iter().skip(off).copied().collect()
    }
}

/// Short-time spectrum of the incoming audio, bucketed into `BANDS`.
struct Analyzer {
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<realfft::num_complex::Complex<f32>>,
    edges: [usize; BANDS + 1],
}

impl Analyzer {
    fn new(sample_rate: u32) -> Self {
        let fft = realfft::RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let spectrum = fft.make_output_vec();

        // Hann window. Without it the hard edges of each frame smear energy
        // across every bin and all seven bands move together as one.
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = i as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * x).cos()
            })
            .collect();

        // Log-spaced edges. Linear spacing would hand six bands to sibilance
        // and one to everything that actually carries the voice.
        let to_bin = |f: f32| ((f * FFT_SIZE as f32 / sample_rate as f32) as usize).min(FFT_SIZE / 2);
        let mut edges = [0usize; BANDS + 1];
        for (i, edge) in edges.iter_mut().enumerate() {
            let t = i as f32 / BANDS as f32;
            *edge = to_bin(F_LOW * (F_HIGH / F_LOW).powf(t));
        }

        Self { fft, window, input: vec![0.0; FFT_SIZE], spectrum, edges }
    }

    fn analyse(&mut self, tail: &[f32]) -> [f32; BANDS] {
        let n = tail.len().min(FFT_SIZE);
        if n == 0 {
            return [0.0; BANDS];
        }
        // Right-align the newest samples; zero-padding the front is harmless.
        self.input.fill(0.0);
        self.input[FFT_SIZE - n..].copy_from_slice(&tail[tail.len() - n..]);
        for (s, w) in self.input.iter_mut().zip(&self.window) {
            *s *= w;
        }
        if self.fft.process(&mut self.input, &mut self.spectrum).is_err() {
            return [0.0; BANDS];
        }

        let mut out = [0.0f32; BANDS];
        for b in 0..BANDS {
            let lo = self.edges[b];
            let hi = self.edges[b + 1].max(lo + 1).min(self.spectrum.len());
            let count = (hi - lo).max(1) as f32;
            let power: f32 = self.spectrum[lo..hi].iter().map(|c| c.norm_sqr()).sum();
            let mag = (power / count).sqrt() / (FFT_SIZE as f32 * 0.25);
            // Work in dB: linear magnitude spends most of its range on silence,
            // so a linear reading barely moves for ordinary speech.
            let db = 20.0 * (mag + 1e-9).log10();
            // Window on the range speech actually occupies. A wider one (-55..0)
            // maps normal talking into the middle third and the visualiser looks
            // inert; -48..-12 spends the full range where the voice lives.
            let norm = ((db + 48.0) / 36.0).clamp(0.0, 1.0);
            // Slight expansion for contrast between syllables and silence.
            out[b] = norm.powf(1.4);
        }
        out
    }
}

pub struct Recorder {
    ring: Arc<Mutex<Ring>>,
    analyzer: Mutex<Analyzer>,
    sample_rate: u32,
    _stream: cpal::Stream,
}

/// Input device names for the settings picker.
pub fn input_devices() -> Vec<String> {
    // cpal 0.18 dropped Device::name(); DeviceTrait requires Display, and that
    // is the stable way to get a human-readable name across backends.
    cpal::default_host()
        .input_devices()
        .map(|ds| ds.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

impl Recorder {
    /// `wanted` names a specific input device; None uses the system default.
    /// A named device that has since been unplugged falls back to the default
    /// rather than refusing to start.
    pub fn new(wanted: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = wanted
            .and_then(|name| {
                host.input_devices()
                    .ok()
                    .and_then(|mut ds| ds.find(|d| d.to_string() == name))
            })
            .or_else(|| host.default_input_device())
            .ok_or_else(|| anyhow!("no input device"))?;
        let supported = device.default_input_config()?;

        let sample_rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        let fmt = supported.sample_format();
        let config = supported.config();

        let ring = Arc::new(Mutex::new(Ring {
            buf: VecDeque::with_capacity(sample_rate as usize * RING_SECS),
            written: 0,
            cap: sample_rate as usize * RING_SECS,
        }));

        let err_fn = |e| eprintln!("audio stream error: {e}");

        // Downmix to mono in the callback so the ring is channel-count agnostic.
        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let ring = Arc::clone(&ring);
                let conv: fn($t) -> f32 = $conv;
                device.build_input_stream(
                    config.clone(),
                    move |data: &[$t], _: &cpal::InputCallbackInfo| {
                        let Ok(mut r) = ring.lock() else { return };
                        for frame in data.chunks(channels) {
                            let sum: f32 = frame.iter().map(|&s| conv(s)).sum();
                            r.push(sum / channels as f32);
                        }
                    },
                    err_fn,
                    None,
                )?
            }};
        }

        let stream = match fmt {
            SampleFormat::F32 => build!(f32, |s| s),
            SampleFormat::I16 => build!(i16, |s| s as f32 / i16::MAX as f32),
            SampleFormat::U16 => build!(u16, |s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0),
            other => return Err(anyhow!("unsupported sample format: {other:?}")),
        };
        stream.play()?;

        crate::log!("mic: {} ch @ {} Hz ({:?})", channels, sample_rate, fmt);

        Ok(Self {
            ring,
            analyzer: Mutex::new(Analyzer::new(sample_rate)),
            sample_rate,
            _stream: stream,
        })
    }

    /// Per-band energy of the most recent audio, one value per ribbon.
    pub fn bands(&self) -> [f32; BANDS] {
        let tail: Vec<f32> = {
            let r = self.ring.lock().unwrap();
            let skip = r.buf.len().saturating_sub(FFT_SIZE);
            r.buf.iter().skip(skip).copied().collect()
        };
        self.analyzer.lock().unwrap().analyse(&tail)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Absolute index to start from, reaching `PREROLL_MS` into the past.
    pub fn mark(&self) -> u64 {
        let preroll = (self.sample_rate as u64 * PREROLL_MS as u64) / 1000;
        let r = self.ring.lock().unwrap();
        r.written.saturating_sub(preroll)
    }

    pub fn take_since(&self, abs: u64) -> Vec<f32> {
        self.ring.lock().unwrap().since(abs)
    }

}

/// Resample to 16kHz mono for whisper.
///
/// Uses a real polyphase resampler rather than dropping samples: naive decimation
/// aliases high-frequency content down into the speech band, which measurably hurts
/// recognition. Costs one dependency and about 20 lines.
pub fn to_16k(input: &[f32], from_rate: u32) -> Result<Vec<f32>> {
    if from_rate == TARGET_RATE {
        return Ok(input.to_vec());
    }
    use rubato::{FftFixedIn, Resampler};

    let chunk = 1024;
    let mut rs = FftFixedIn::<f32>::new(from_rate as usize, TARGET_RATE as usize, chunk, 2, 1)?;

    let mut out = Vec::with_capacity(input.len() * TARGET_RATE as usize / from_rate as usize);
    let mut pos = 0;
    while pos + chunk <= input.len() {
        let frames = rs.process(&[&input[pos..pos + chunk]], None)?;
        out.extend_from_slice(&frames[0]);
        pos += chunk;
    }
    // Flush the remainder; process_partial zero-pads internally.
    if pos < input.len() {
        let frames = rs.process_partial(Some(&[&input[pos..]]), None)?;
        out.extend_from_slice(&frames[0]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(cap: usize) -> Ring {
        Ring {
            buf: VecDeque::new(),
            written: 0,
            cap,
        }
    }

    #[test]
    fn marks_survive_rotation() {
        // The whole point of absolute indexing: a mark taken before the ring
        // wrapped must still resolve to the right samples afterwards.
        let mut r = ring(4);
        for i in 0..3 {
            r.push(i as f32);
        }
        let mark = r.written - 2; // expect to get back [1.0, 2.0]
        assert_eq!(r.since(mark), vec![1.0, 2.0]);

        for i in 3..6 {
            r.push(i as f32); // rotates: buf is now [2,3,4,5]
        }
        // Same mark, ring has moved. 1.0 is gone; we get everything still held.
        assert_eq!(r.since(mark), vec![2.0, 3.0, 4.0, 5.0]);
        assert_eq!(r.buf.len(), 4, "capacity must be respected");
    }

    #[test]
    fn mark_older_than_ring_clamps() {
        let mut r = ring(2);
        for i in 0..5 {
            r.push(i as f32);
        }
        assert_eq!(r.since(0), vec![3.0, 4.0], "must not panic or overshoot");
    }

    #[test]
    fn resample_lands_near_target_rate() {
        // One second of 48k in, expect ~one second of 16k out.
        let input: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 0.01).sin() * 0.5)
            .collect();
        let out = to_16k(&input, 48_000).unwrap();
        let drift = (out.len() as i64 - 16_000).abs();
        assert!(drift < 500, "got {} samples, expected ~16000", out.len());
    }

    #[test]
    fn resample_is_a_noop_at_target_rate() {
        let input = vec![0.1, -0.2, 0.3];
        assert_eq!(to_16k(&input, TARGET_RATE).unwrap(), input);
    }
}
