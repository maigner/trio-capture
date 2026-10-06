//! WAV playback through cpal. The number of frames handed to the device,
//! less what the device has not played yet, is the master clock for the
//! whole timeline.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

struct Shared {
    pcm: RwLock<Arc<Vec<f32>>>,
    /// Position in frames (one frame = one sample per channel) handed to
    /// the device.
    pos: AtomicU64,
    /// Frames handed to the device that it has not played yet, as the
    /// device reports it: a USB interface or Bluetooth headphones sit a
    /// good fraction of a second behind, and the picture must not run
    /// ahead of the sound by that.
    latency: AtomicU64,
    /// Frame of the last seek; the heard position never reads below it.
    floor: AtomicU64,
    playing: AtomicBool,
    channels: usize,
}

pub struct Player {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
    rate: u32,
    channels: u16,
}

impl Player {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no audio output device"))?;
        let config = device
            .default_output_config()
            .context("querying output config")?;
        let rate = config.sample_rate();
        let channels = config.channels();
        let stream_config: cpal::StreamConfig = config.into();

        let shared = Arc::new(Shared {
            pcm: RwLock::new(Arc::new(Vec::new())),
            pos: AtomicU64::new(0),
            latency: AtomicU64::new(0),
            floor: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            channels: channels as usize,
        });
        let cb_shared = shared.clone();
        let rate_f = rate as f64;
        let stream = device
            .build_output_stream(
                stream_config,
                move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    let ts = info.timestamp();
                    let d = ts.playback.duration_since(ts.callback);
                    cb_shared
                        .latency
                        .store((d.as_secs_f64() * rate_f) as u64, Ordering::Relaxed);
                    fill(&cb_shared, out)
                },
                |e| tracing::error!("audio stream error: {e}"),
                None,
            )
            .context("building output stream")?;
        stream.play().context("starting output stream")?;
        Ok(Self {
            shared,
            _stream: stream,
            rate,
            channels,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// `pcm` must be interleaved at [`Self::sample_rate`] / [`Self::channels`].
    pub fn set_pcm(&self, pcm: Arc<Vec<f32>>) {
        *self.shared.pcm.write().unwrap() = pcm;
        self.shared.pos.store(0, Ordering::SeqCst);
        self.shared.floor.store(0, Ordering::SeqCst);
    }

    pub fn has_audio(&self) -> bool {
        !self.shared.pcm.read().unwrap().is_empty()
    }

    pub fn duration(&self) -> f64 {
        let n = self.shared.pcm.read().unwrap().len();
        n as f64 / (self.rate as f64 * self.channels as f64)
    }

    pub fn play(&self) {
        self.shared.playing.store(true, Ordering::SeqCst);
    }
    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::SeqCst);
    }
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::SeqCst)
    }
    pub fn seek(&self, seconds: f64) {
        let frame = (seconds.max(0.0) * self.rate as f64) as u64;
        self.shared.pos.store(frame, Ordering::SeqCst);
        self.shared.floor.store(frame, Ordering::SeqCst);
    }
    /// What is coming out of the speakers right now: frames handed to the
    /// device less its reported output latency.
    pub fn position(&self) -> f64 {
        let handed = self.shared.pos.load(Ordering::SeqCst);
        let latency = self.shared.latency.load(Ordering::Relaxed);
        let floor = self.shared.floor.load(Ordering::SeqCst);
        handed.saturating_sub(latency).max(floor) as f64 / self.rate as f64
    }
    /// Output latency the device reports, in seconds.
    pub fn latency(&self) -> f64 {
        self.shared.latency.load(Ordering::Relaxed) as f64 / self.rate as f64
    }
}

fn fill(shared: &Shared, out: &mut [f32]) {
    if !shared.playing.load(Ordering::Relaxed) {
        out.fill(0.0);
        return;
    }
    let pcm = shared.pcm.read().unwrap().clone();
    let ch = shared.channels;
    let mut pos = shared.pos.load(Ordering::Relaxed) as usize * ch;
    for frame in out.chunks_mut(ch) {
        if pos + ch <= pcm.len() {
            frame.copy_from_slice(&pcm[pos..pos + ch]);
        } else {
            frame.fill(0.0);
        }
        pos += ch;
    }
    shared.pos.store((pos / ch) as u64, Ordering::Relaxed);
}
