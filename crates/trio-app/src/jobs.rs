//! Background work: folder scans, WAV loading, sync. Results come back
//! through a channel and are applied on the UI thread.

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::path::PathBuf;
use std::sync::Arc;
use trio_core::discover::scan_folder;
use trio_core::sync::{Arranged, Master, SYNC_RATE};
use trio_core::{Camera, Clip, Grade, Project};
use trio_media::audio::decode_pcm;
use trio_media::ffmpeg::{detect_hwaccel, HwAccel};

pub struct WavData {
    pub mono8k: Arc<Vec<f32>>,
    /// Interleaved at the player's rate/channels, empty when no player exists.
    pub playback: Arc<Vec<f32>>,
    pub duration: f64,
}

pub enum JobResult {
    Scanned {
        cam: usize,
        folder: PathBuf,
        result: Result<Vec<Clip>>,
    },
    WavLoaded(Result<WavData>),
    /// One clip's audio has been analysed (progress only).
    SyncProgress,
    Synced {
        cam: usize,
        index: usize,
        result: Arranged,
    },
    SyncFinished,
    HwAccel(HwAccel),
    /// One sample frame has been analysed for the auto grade (progress only).
    GradeProgress,
    Graded(Result<Vec<Grade>>),
}

pub struct JobHub {
    tx: Sender<JobResult>,
    rx: Receiver<JobResult>,
    pub running: usize,
    ctx: egui::Context,
}

impl JobHub {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            running: 0,
            ctx,
        }
    }

    pub fn poll(&mut self) -> Vec<JobResult> {
        let out: Vec<_> = self.rx.try_iter().collect();
        for r in &out {
            if !matches!(
                r,
                JobResult::Synced { .. } | JobResult::SyncProgress | JobResult::GradeProgress
            ) {
                self.running = self.running.saturating_sub(1);
            }
        }
        out
    }

    fn spawn<F: FnOnce(&Sender<JobResult>) + Send + 'static>(&mut self, f: F) {
        self.running += 1;
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            f(&tx);
            ctx.request_repaint();
        });
    }

    pub fn scan(&mut self, cam: usize, folder: PathBuf) {
        self.spawn(move |tx| {
            let result = scan_folder(&folder);
            let _ = tx.send(JobResult::Scanned {
                cam,
                folder,
                result,
            });
        });
    }

    pub fn load_wav(&mut self, path: PathBuf, playback_rate: Option<(u32, u16)>) {
        self.spawn(move |tx| {
            let result = (|| -> Result<WavData> {
                let mono8k = decode_pcm(&path, SYNC_RATE, 1)?;
                let duration = mono8k.len() as f64 / SYNC_RATE as f64;
                let playback = match playback_rate {
                    Some((rate, ch)) => decode_pcm(&path, rate, ch)?,
                    None => Vec::new(),
                };
                Ok(WavData {
                    mono8k: Arc::new(mono8k),
                    playback: Arc::new(playback),
                    duration,
                })
            })();
            let _ = tx.send(JobResult::WavLoaded(result));
        });
    }

    pub fn sync(&mut self, master: Arc<Vec<f32>>, cameras: Vec<Camera>) {
        let ctx = self.ctx.clone();
        self.spawn(move |tx| {
            let master = Master::new(master);
            let results = trio_media::sync::sync_cameras(&master, &cameras, &|_, _| {
                let _ = tx.send(JobResult::SyncProgress);
                ctx.request_repaint();
            });
            for (cam, res) in results.into_iter().enumerate() {
                for (index, result) in res.into_iter().enumerate() {
                    let _ = tx.send(JobResult::Synced { cam, index, result });
                }
            }
            let _ = tx.send(JobResult::SyncFinished);
        });
    }

    pub fn auto_grade(&mut self, project: Project, hwaccel: HwAccel) {
        let ctx = self.ctx.clone();
        self.spawn(move |tx| {
            let result = trio_media::grade::auto_grade(&project, hwaccel, &|| {
                let _ = tx.send(JobResult::GradeProgress);
                ctx.request_repaint();
            });
            let _ = tx.send(JobResult::Graded(result));
        });
    }

    pub fn detect_hwaccel(&mut self, sample: PathBuf) {
        self.spawn(move |tx| {
            let _ = tx.send(JobResult::HwAccel(detect_hwaccel(&sample)));
        });
    }
}
