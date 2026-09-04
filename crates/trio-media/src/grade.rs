//! Automatic grade for a project: decode a few small frames per camera at
//! moments every camera covers, then let trio-core derive matching grades.

use crate::decoder::{grab_frame, DecodeRequest};
use crate::ffmpeg::{fit_size, HwAccel};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::sync::Mutex;
use trio_core::autograde::{sample_times, solve, visible_region, CameraStats, SAMPLE_COUNT};
use trio_core::{Grade, Project, CAMERA_COUNT};

/// Longest edge of the analysis frames; statistics need no detail.
const ANALYSIS_EDGE: u32 = 320;

/// One grade per camera. `on_frame` is called from worker threads as each
/// sample frame has been analysed.
pub fn auto_grade(
    project: &Project,
    hwaccel: HwAccel,
    on_frame: &(dyn Fn() + Sync),
) -> Result<Vec<Grade>> {
    let times = sample_times(project, SAMPLE_COUNT);
    if times.is_empty() {
        return Err(anyhow!("no clips on the timeline to analyse"));
    }
    let mut jobs = Vec::new();
    for cam in 0..CAMERA_COUNT {
        for &t in &times {
            if let Some((_, clip)) = project.clip_at(cam, t) {
                let (width, height) =
                    fit_size(clip.width, clip.height, ANALYSIS_EDGE, ANALYSIS_EDGE);
                // Stay clear of the very end, where a seek may find no frame.
                let local = (t - clip.offset).min(clip.duration - 0.5).max(0.0);
                jobs.push((
                    cam,
                    visible_region(project, cam, clip.width, clip.height),
                    DecodeRequest {
                        path: clip.path.clone(),
                        start: local,
                        fps: project.output.fps,
                        width,
                        height,
                        hdr: clip.hdr,
                        hwaccel,
                    },
                ));
            }
        }
    }
    let stats: Mutex<Vec<CameraStats>> = Mutex::new(vec![CameraStats::new(); CAMERA_COUNT]);
    jobs.par_iter().for_each(|(cam, region, req)| {
        match grab_frame(req) {
            Ok(f) => stats.lock().unwrap()[*cam].add_region(&f.rgba, f.width, f.height, *region),
            Err(e) => tracing::warn!("auto grade: {e:#}"),
        }
        on_frame();
    });
    let stats = stats.into_inner().unwrap();
    for (cam, s) in stats.iter().enumerate() {
        tracing::info!(
            "auto grade: cam {cam}: {} frames analysed, {}",
            s.frames,
            s.summary()
        );
    }
    if stats.iter().all(|s| s.is_empty()) {
        return Err(anyhow!("no frames could be decoded for analysis"));
    }
    Ok(solve(&stats))
}

/// Number of frames `auto_grade` will analyse, for progress display.
pub fn sample_total(project: &Project) -> usize {
    sample_times(project, SAMPLE_COUNT)
        .iter()
        .map(|&t| {
            (0..CAMERA_COUNT)
                .filter(|&cam| project.clip_at(cam, t).is_some())
                .count()
        })
        .sum()
}
