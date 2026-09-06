//! Find the people in every camera: decode a few small frames a moment
//! apart at sample moments over the whole timeline, then let trio-core
//! work out where the movement (and, in white light, the skin) is.

use crate::decoder::{grab_frames, DecodeRequest};
use crate::ffmpeg::{fit_size, HwAccel};
use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::sync::Mutex;
use trio_core::autoframe::{analyse, sample_times, FRAMES_PER_SAMPLE, FRAME_GAP};
use trio_core::{Project, Subject, SubjectSample, CAMERA_COUNT};

/// Longest edge of the analysis frames; a coarse grid is all that is needed.
const ANALYSIS_EDGE: u32 = 320;

/// Decode requests for every sample moment a camera covers.
fn jobs(project: &Project, hwaccel: HwAccel) -> Vec<(usize, f64, DecodeRequest)> {
    let span = FRAME_GAP * (FRAMES_PER_SAMPLE - 1) as f64;
    let mut jobs = Vec::new();
    for cam in 0..CAMERA_COUNT {
        for t in sample_times(project.duration(None)) {
            if let Some((_, clip)) = project.clip_at(cam, t) {
                let (width, height) =
                    fit_size(clip.width, clip.height, ANALYSIS_EDGE, ANALYSIS_EDGE);
                // Every frame must exist, and a seek near the end may find none.
                let local = (t - clip.offset).min(clip.duration - span - 0.5).max(0.0);
                if local + span > clip.duration {
                    continue;
                }
                jobs.push((
                    cam,
                    t,
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
    jobs
}

/// One subject per camera, `None` where nothing ever moved. `on_sample` is
/// called from worker threads as each sample moment has been analysed.
pub fn find_subjects(
    project: &Project,
    hwaccel: HwAccel,
    on_sample: &(dyn Fn() + Sync),
) -> Result<Vec<Option<Subject>>> {
    let jobs = jobs(project, hwaccel);
    if jobs.is_empty() {
        return Err(anyhow!("no clips on the timeline to analyse"));
    }
    let samples: Mutex<Vec<Vec<SubjectSample>>> = Mutex::new(vec![Vec::new(); CAMERA_COUNT]);
    let decoded = std::sync::atomic::AtomicUsize::new(0);
    jobs.par_iter().for_each(|(cam, t, req)| {
        match grab_frames(req, FRAMES_PER_SAMPLE, FRAME_GAP) {
            Ok(frames) => {
                decoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let rgba: Vec<&[u8]> = frames.iter().map(|f| f.rgba.as_slice()).collect();
                if let Some((faces, people)) = analyse(&rgba, req.width, req.height) {
                    samples.lock().unwrap()[*cam].push(SubjectSample {
                        t: *t,
                        faces,
                        people,
                    });
                }
            }
            Err(e) => tracing::warn!("find subjects: {e:#}"),
        }
        on_sample();
    });
    if decoded.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        return Err(anyhow!("no frames could be decoded for analysis"));
    }
    let samples = samples.into_inner().unwrap();
    Ok(samples
        .into_iter()
        .enumerate()
        .map(|(cam, mut s)| {
            s.sort_by(|a, b| a.t.total_cmp(&b.t));
            let subject = (!s.is_empty()).then_some(Subject { samples: s });
            tracing::info!(
                "find subjects: cam {cam}: {} samples, faces overall {:?}",
                subject.as_ref().map_or(0, |s| s.samples.len()),
                subject.as_ref().and_then(|s| s.faces_overall())
            );
            subject
        })
        .collect())
}

/// Number of sample moments `find_subjects` will analyse, for progress display.
pub fn sample_total(project: &Project) -> usize {
    jobs(project, HwAccel::None).len()
}
