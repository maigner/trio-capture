//! Incremental export driven from the UI thread so progress stays visible.

use crate::engine::{Mode, Quality, StreamSet};
use crate::gpu::{Compositor, Target};
use anyhow::Result;
use std::path::Path;
use std::time::{Duration, Instant};
use trio_core::Project;
use trio_media::export::{Encoder, ExportSpec};
use trio_media::ffmpeg::HwAccel;

pub const EXPORT_MAX_EDGE: u32 = 4096;

pub fn export_spec_for(
    project: &Project,
    out: &Path,
    duration: f64,
    hwaccel: HwAccel,
) -> ExportSpec {
    let (w, h) = project.output_size();
    ExportSpec {
        width: w,
        height: h,
        fps: project.output.fps,
        codec: hwaccel.resolve_codec(&project.output),
        quality: project.output.quality,
        out_path: out.to_path_buf(),
        wav: project.wav.clone(),
        audio_start: project.range.start,
        duration,
    }
}

pub struct ExportJob {
    project: Project,
    encoder: Option<Encoder>,
    streams: StreamSet,
    target: Target,
    uploaded: [u64; 3],
    frame: u64,
    total: u64,
    fps: f64,
    started: Instant,
    pub error: Option<String>,
    pub done: bool,
    pub out_path: std::path::PathBuf,
}

impl ExportJob {
    pub fn start(
        comp: &Compositor,
        project: &Project,
        hwaccel: HwAccel,
        duration: f64,
    ) -> Result<Self> {
        let out = project
            .output
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no output path"))?;
        let spec = export_spec_for(project, &out, duration, hwaccel);
        let encoder = Encoder::start(&spec)?;
        let target = comp.create_target(spec.width, spec.height);
        Ok(Self {
            project: project.clone(),
            encoder: Some(encoder),
            streams: StreamSet::new(
                Quality::Full {
                    max_edge: EXPORT_MAX_EDGE,
                },
                hwaccel,
                spec.fps,
            ),
            target,
            uploaded: [u64::MAX; 3],
            frame: 0,
            total: (duration * spec.fps).round().max(1.0) as u64,
            fps: spec.fps,
            started: Instant::now(),
            error: None,
            done: false,
            out_path: out,
        })
    }

    pub fn progress(&self) -> f32 {
        self.frame as f32 / self.total as f32
    }

    pub fn status(&self) -> String {
        let el = self.started.elapsed().as_secs_f64();
        let rate = self.frame as f64 / el.max(1e-3);
        let eta = if rate > 0.0 {
            (self.total - self.frame) as f64 / rate
        } else {
            0.0
        };
        format!(
            "{} / {} frames, {:.1} fps, ETA {:.0}s",
            self.frame, self.total, rate, eta
        )
    }

    /// Encode frames for up to `budget`; returns true when finished.
    pub fn step(&mut self, comp: &Compositor, budget: Duration) -> bool {
        if self.done {
            return true;
        }
        let t0 = Instant::now();
        while self.frame < self.total && t0.elapsed() < budget {
            let t = self.project.range.start + self.frame as f64 / self.fps;
            self.streams.advance(&self.project, t, Mode::Exact);
            for cam in 0..3 {
                if self.uploaded[cam] != self.streams.generation(cam) {
                    self.uploaded[cam] = self.streams.generation(cam);
                    match self.streams.current(cam) {
                        Some(fr) => {
                            comp.upload(&mut self.target, cam, fr.width, fr.height, &fr.rgba)
                        }
                        None => comp.clear_source(&mut self.target, cam),
                    }
                }
            }
            comp.render(&self.target, &self.project);
            let bytes = comp.readback(&mut self.target);
            if let Err(e) = self.encoder.as_mut().unwrap().write_frame(&bytes) {
                self.error = Some(format!("{e:#}"));
                self.done = true;
                return true;
            }
            self.frame += 1;
        }
        if self.frame >= self.total {
            if let Some(enc) = self.encoder.take() {
                if let Err(e) = enc.finish() {
                    self.error = Some(format!("{e:#}"));
                }
            }
            self.done = true;
        }
        self.done
    }
}
