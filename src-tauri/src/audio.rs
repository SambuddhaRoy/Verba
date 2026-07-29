//! Continuous microphone capture into a ring buffer.
//!
//! The stream runs for the life of the process rather than starting on keypress.
//! Two reasons: opening a WASAPI stream costs 50-200ms, which would clip the start
//! of every utterance; and a always-full ring lets us reach *backwards* for pre-roll,
//! catching the word you started saying as you pressed the key.

use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

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

pub struct Recorder {
    ring: Arc<Mutex<Ring>>,
    sample_rate: u32,
    _stream: cpal::Stream,
}

impl Recorder {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
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

        println!("mic: {} ch @ {} Hz ({:?})", channels, sample_rate, fmt);

        Ok(Self {
            ring,
            sample_rate,
            _stream: stream,
        })
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
