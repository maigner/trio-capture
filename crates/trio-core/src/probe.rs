//! Thin wrapper around `ffprobe`.

use crate::model::Clip;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub fn ffprobe_path() -> String {
    std::env::var("TRIO_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

pub fn probe_clip(path: &Path) -> Result<Clip> {
    let out = Command::new(ffprobe_path())
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .with_context(|| format!("running ffprobe on {}", path.display()))?;
    if !out.status.success() {
        return Err(anyhow!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let v: Value = serde_json::from_slice(&out.stdout).context("parsing ffprobe json")?;
    parse_probe(path, &v)
}

fn parse_probe(path: &Path, v: &Value) -> Result<Clip> {
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| anyhow!("no video stream in {}", path.display()))?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");

    let w = video["width"].as_u64().unwrap_or(0) as u32;
    let h = video["height"].as_u64().unwrap_or(0) as u32;
    let rotation = parse_rotation(video);
    let (width, height) = if rotation.abs() % 180 == 90 {
        (h, w)
    } else {
        (w, h)
    };

    let fps = parse_rate(video["avg_frame_rate"].as_str())
        .or_else(|| parse_rate(video["r_frame_rate"].as_str()))
        .unwrap_or(30.0);

    let duration = v["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| video["duration"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0);

    let transfer = video["color_transfer"].as_str().unwrap_or("");
    let hdr = matches!(transfer, "arib-std-b67" | "smpte2084");

    let creation_time = v["format"]["tags"]["creation_time"]
        .as_str()
        .or_else(|| video["tags"]["creation_time"].as_str())
        .map(|s| s.to_string());
    let end_stamped = v["format"]["tags"]["com.android.version"].is_string();

    Ok(Clip {
        path: path.to_path_buf(),
        duration,
        width,
        height,
        fps,
        rotation,
        hdr,
        has_audio,
        creation_time,
        end_stamped,
        offset: 0.0,
        sync_confidence: None,
    })
}

fn parse_rotation(video: &Value) -> i32 {
    if let Some(list) = video["side_data_list"].as_array() {
        for sd in list {
            if let Some(r) = sd["rotation"].as_f64() {
                return r.round() as i32;
            }
        }
    }
    video["tags"]["rotate"]
        .as_str()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0)
}

fn parse_rate(s: Option<&str>) -> Option<f64> {
    let s = s?;
    let (n, d) = s.split_once('/')?;
    let n: f64 = n.parse().ok()?;
    let d: f64 = d.parse().ok()?;
    if d == 0.0 || n == 0.0 {
        None
    } else {
        Some(n / d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_rotated_hdr_clip() {
        let v = json!({
            "streams": [
                {"codec_type": "video", "width": 3840, "height": 2160, "avg_frame_rate": "30000/1001",
                 "color_transfer": "arib-std-b67",
                 "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}],
                 "tags": {"creation_time": "2026-09-01T20:00:00.000000Z"}},
                {"codec_type": "audio"}
            ],
            "format": {"duration": "12.5", "tags": {}}
        });
        let c = parse_probe(Path::new("x.mov"), &v).unwrap();
        assert_eq!((c.width, c.height), (2160, 3840));
        assert!(c.hdr);
        assert!(c.has_audio);
        assert!((c.fps - 29.97).abs() < 0.01);
        assert_eq!(c.duration, 12.5);
        assert!(c.creation_time.is_some());
    }
}
