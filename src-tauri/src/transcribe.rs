//! Background transcription worker.
//!
//! Lives on its own thread so interim passes during dictation never stall the
//! engine loop, which emits band energy at 30fps while a whisper pass takes
//! 150-400ms. Owning the model here also keeps load and unload in one place.

use anyhow::Result;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::{config, stt};

pub enum Job {
    Transcribe {
        pcm: Vec<f32>,
        /// Utterance number. Results carrying a stale generation are dropped,
        /// so a slow interim pass from the previous dictation cannot land in
        /// the next one.
        utterance: u64,
        /// Final passes are definitive and must never be coalesced away.
        final_pass: bool,
    },
    /// Drop the model and free its memory.
    Unload,
}

pub enum Done {
    Partial { text: String, utterance: u64 },
    Final { text: String, utterance: u64, took: Duration },
    Failed(String),
}

pub struct Worker {
    pub jobs: Sender<Job>,
    pub done: Receiver<Done>,
}

/// Interim passes only ever look at the tail. Re-running whisper over a
/// steadily growing buffer is O(n) per pass, so a long dictation would slow
/// down as it went; the final pass still sees everything.
const PARTIAL_TAIL_SECS: usize = 20;

pub fn spawn() -> Result<Worker> {
    let (job_tx, job_rx) = channel::<Job>();
    let (done_tx, done_rx) = channel::<Done>();

    std::thread::Builder::new()
        .name("transcribe".into())
        .spawn(move || run(job_rx, done_tx))?;

    Ok(Worker { jobs: job_tx, done: done_rx })
}

fn run(jobs: Receiver<Job>, done: Sender<Done>) {
    let mut engine: Option<stt::Engine> = None;
    let mut loaded = String::new();

    while let Ok(first) = jobs.recv() {
        // Coalesce a backlog. If passes are queuing faster than they complete,
        // only the newest audio is worth transcribing — except that a final
        // pass outranks any interim one, whenever it arrived.
        let mut job = first;
        loop {
            match jobs.try_recv() {
                Ok(next) => {
                    let holding_final =
                        matches!(job, Job::Transcribe { final_pass: true, .. });
                    let next_is_partial =
                        matches!(next, Job::Transcribe { final_pass: false, .. });
                    if holding_final && next_is_partial {
                        continue;
                    }
                    job = next;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }

        let (pcm, utterance, final_pass) = match job {
            Job::Unload => {
                if engine.is_some() {
                    engine = None;
                    loaded.clear();
                    crate::log!("model unloaded");
                }
                continue;
            }
            Job::Transcribe { pcm, utterance, final_pass } => (pcm, utterance, final_pass),
        };

        let cfg = config::load();
        if engine.is_none() || loaded != cfg.model {
            let path = match crate::model_path(&cfg.model) {
                Ok(p) => p,
                Err(e) => {
                    let _ = done.send(Done::Failed(e.to_string()));
                    continue;
                }
            };
            crate::log!("  loading {}", cfg.model);
            match stt::Engine::new(&path, cfg.threads) {
                Ok(e) => {
                    engine = Some(e);
                    loaded = cfg.model.clone();
                }
                Err(e) => {
                    let _ = done.send(Done::Failed(e.to_string()));
                    continue;
                }
            }
        }
        let Some(eng) = engine.as_ref() else { continue };

        let slice: &[f32] = if final_pass {
            &pcm
        } else {
            let cap = PARTIAL_TAIL_SECS * audio_rate();
            &pcm[pcm.len().saturating_sub(cap)..]
        };

        let t0 = Instant::now();
        match eng.transcribe(slice, !final_pass) {
            Ok(text) => {
                let _ = done.send(if final_pass {
                    Done::Final { text, utterance, took: t0.elapsed() }
                } else {
                    Done::Partial { text, utterance }
                });
            }
            Err(e) => {
                let _ = done.send(Done::Failed(e.to_string()));
            }
        }
    }
}

/// Audio reaching the worker is already resampled for whisper.
fn audio_rate() -> usize {
    crate::audio::TARGET_RATE as usize
}
