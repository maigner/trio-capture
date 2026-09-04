//! Find and order the clips of one camera folder.

use crate::model::{Clip, CAMERA_COUNT};
use crate::probe::probe_clip;
use anyhow::Result;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "3gp", "webm"];
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "flac", "aif", "aiff", "mp3", "m4a"];

pub fn is_video_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn is_audio_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// What a shoot folder contains: up to three camera subfolders and the
/// master audio file that lives directly in the folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shoot {
    pub root: PathBuf,
    /// Camera folders in name order, at most `CAMERA_COUNT`.
    pub cameras: Vec<PathBuf>,
    pub wav: Option<PathBuf>,
    /// Extra folders with video files that did not fit the three slots.
    pub skipped_cameras: Vec<PathBuf>,
    /// Other audio files in the root that were not chosen.
    pub other_audio: Vec<PathBuf>,
}

/// Look at `root` without probing anything: every direct subfolder holding at
/// least one video file is a camera, and the audio file in `root` is the
/// master. Camera folders are sorted by name so `Cam1`, `Cam2`, `Cam3` land
/// in order; with more than three, folders whose name contains "cam" win.
/// Among several audio files a `.wav` is preferred, then the largest.
pub fn discover_shoot(root: &Path) -> Result<Shoot> {
    let mut cameras = Vec::new();
    let mut audio = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(true);
        if hidden {
            continue;
        }
        if path.is_dir() {
            let has_video = std::fs::read_dir(&path)
                .map(|it| {
                    it.filter_map(|e| e.ok())
                        .any(|e| e.path().is_file() && is_video_file(&e.path()))
                })
                .unwrap_or(false);
            if has_video {
                cameras.push(path);
            }
        } else if path.is_file() && is_audio_file(&path) {
            audio.push(path);
        }
    }
    cameras.sort();
    let looks_like_cam = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_ascii_lowercase().contains("cam"))
            .unwrap_or(false)
    };
    if cameras.len() > CAMERA_COUNT && cameras.iter().filter(|p| looks_like_cam(p)).count() > 0 {
        let (named, rest): (Vec<_>, Vec<_>) = cameras.into_iter().partition(|p| looks_like_cam(p));
        cameras = named;
        cameras.extend(rest);
    }
    let skipped_cameras = cameras.split_off(cameras.len().min(CAMERA_COUNT));

    audio.sort_by(|a, b| {
        let is_wav = |p: &Path| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("wav"))
                .unwrap_or(false)
        };
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        is_wav(b)
            .cmp(&is_wav(a))
            .then_with(|| size(b).cmp(&size(a)))
            .then_with(|| a.cmp(b))
    });
    let mut audio = audio.into_iter();
    let wav = audio.next();
    Ok(Shoot {
        root: root.to_path_buf(),
        cameras,
        wav,
        skipped_cameras,
        other_audio: audio.collect(),
    })
}

/// Probe every video file in `folder` (non-recursive) and order the clips
/// by creation time, then file name. Offsets are laid out back to back from
/// creation times so a rough timeline exists before sync runs.
pub fn scan_folder(folder: &Path) -> Result<Vec<Clip>> {
    let mut files: Vec<_> = std::fs::read_dir(folder)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_video_file(p))
        .collect();
    files.sort();

    let mut clips: Vec<Clip> = files
        .par_iter()
        .filter_map(|p| match probe_clip(p) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("skipping {}: {e:#}", p.display());
                None
            }
        })
        .collect();

    clips.sort_by(|a, b| {
        a.creation_time
            .cmp(&b.creation_time)
            .then_with(|| a.path.cmp(&b.path))
    });
    layout_by_creation_time(&mut clips);
    Ok(clips)
}

/// Initial offsets: relative creation times when available, else sequential.
pub fn layout_by_creation_time(clips: &mut [Clip]) {
    let first = clips
        .first()
        .and_then(|c| parse_iso_seconds(c.creation_time.as_deref()?));
    let mut cursor = 0.0;
    for c in clips.iter_mut() {
        let rel = first.and_then(|f| parse_iso_seconds(c.creation_time.as_deref()?).map(|t| t - f));
        // Phones stamp creation_time at the end of recording on some models,
        // so an estimate can go negative; fall back to sequential in that case.
        c.offset = match rel {
            Some(r) if r >= cursor - 1.0 => r.max(0.0),
            _ => cursor,
        };
        cursor = c.offset + c.duration;
    }
}

/// Parses "YYYY-MM-DDTHH:MM:SS(.ffffff)Z" into seconds since an arbitrary epoch.
pub fn parse_iso_seconds(s: &str) -> Option<f64> {
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let hh: i64 = t.next()?.parse().ok()?;
    let mm: i64 = t.next()?.parse().ok()?;
    let ss: f64 = t.next()?.parse().ok()?;
    let days = days_from_civil(y, m, day);
    Some(days as f64 * 86400.0 + hh as f64 * 3600.0 + mm as f64 * 60.0 + ss)
}

// Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn shoot_discovery_picks_cameras_and_wav() {
        let dir = std::env::temp_dir().join(format!("trio-shoot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        touch(&dir.join("Cam1/a.mp4"), 1);
        touch(&dir.join("Cam2/b.MOV"), 1);
        touch(&dir.join("Cam3/c.mp4"), 1);
        touch(&dir.join("Extra/d.mp4"), 1);
        touch(&dir.join("daw project/song.bwproject"), 1);
        touch(&dir.join("daw project/bounce/mix.wav"), 1);
        touch(&dir.join("rough mix.mp3"), 10);
        touch(&dir.join("master take.wav"), 5);
        touch(&dir.join("notes.txt"), 1);
        touch(&dir.join(".hidden/e.mp4"), 1);

        let s = discover_shoot(&dir).unwrap();
        assert_eq!(
            s.cameras,
            vec![dir.join("Cam1"), dir.join("Cam2"), dir.join("Cam3")]
        );
        assert_eq!(s.skipped_cameras, vec![dir.join("Extra")]);
        assert_eq!(s.wav, Some(dir.join("master take.wav")));
        assert_eq!(s.other_audio, vec![dir.join("rough mix.mp3")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn iso_parse_differences() {
        let a = parse_iso_seconds("2026-09-01T20:00:00.000000Z").unwrap();
        let b = parse_iso_seconds("2026-09-01T20:01:30.500000Z").unwrap();
        assert!((b - a - 90.5).abs() < 1e-6);
        let c = parse_iso_seconds("2026-09-02T00:00:00Z").unwrap();
        assert!((c - a - 4.0 * 3600.0).abs() < 1e-6);
    }
}
