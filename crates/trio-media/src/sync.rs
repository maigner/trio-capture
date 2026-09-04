//! Sync every clip of a project against the master WAV: decode each clip's
//! audio, collect candidate offsets, then arrange each camera's clips so
//! they never overlap.

use crate::audio::decode_pcm;
use rayon::prelude::*;
use trio_core::discover::parse_iso_seconds;
use trio_core::sync::{arrange, start_times, Arranged, ClipInfo, Master, SyncResult, SYNC_RATE};
use trio_core::Camera;

/// One camera: results in clip order. `on_clip` is called as each clip's
/// audio has been analysed (from worker threads).
pub fn sync_camera(
    master: &Master,
    cam: &Camera,
    on_clip: &(dyn Fn(usize) + Sync),
) -> Vec<Arranged> {
    let candidates: Vec<Vec<SyncResult>> = cam
        .clips
        .par_iter()
        .enumerate()
        .map(|(i, clip)| {
            let c = match decode_pcm(&clip.path, SYNC_RATE, 1) {
                Ok(pcm) => master.candidates(&pcm),
                Err(e) => {
                    tracing::warn!("sync: cannot decode {}: {e:#}", clip.path.display());
                    Vec::new()
                }
            };
            tracing::debug!("{}: candidates {c:?}", clip.file_name());
            on_clip(i);
            c
        })
        .collect();
    let creation: Vec<Option<f64>> = cam
        .clips
        .iter()
        .map(|c| c.creation_time.as_deref().and_then(parse_iso_seconds))
        .collect();
    let durations: Vec<f64> = cam.clips.iter().map(|c| c.duration).collect();
    let starts = start_times(&creation, &durations);
    let infos: Vec<ClipInfo> = durations
        .iter()
        .zip(&starts)
        .map(|(&duration, &start_time)| ClipInfo {
            duration,
            start_time,
        })
        .collect();
    let current: Vec<f64> = cam.clips.iter().map(|c| c.offset).collect();
    arrange(&infos, &candidates, &current)
}

/// All cameras in parallel; `on_clip(cam, clip_index)` reports progress.
pub fn sync_cameras(
    master: &Master,
    cameras: &[Camera],
    on_clip: &(dyn Fn(usize, usize) + Sync),
) -> Vec<Vec<Arranged>> {
    cameras
        .par_iter()
        .enumerate()
        .map(|(ci, cam)| sync_camera(master, cam, &|i| on_clip(ci, i)))
        .collect()
}
