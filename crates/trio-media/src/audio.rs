//! PCM extraction through ffmpeg (handles any WAV flavour and the AAC
//! tracks inside phone clips alike).

use crate::ffmpeg::ffmpeg_path;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Interleaved f32 samples at `rate` Hz with `channels` channels.
pub fn decode_pcm(path: &Path, rate: u32, channels: u16) -> Result<Vec<f32>> {
    let out = Command::new(ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-sn",
            "-dn",
            "-ac",
            &channels.to_string(),
            "-ar",
            &rate.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("decoding audio of {}", path.display()))?;
    if !out.status.success() {
        return Err(anyhow!(
            "ffmpeg audio decode failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let bytes = out.stdout;
    let mut pcm = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        pcm.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(pcm)
}

/// Min/max pairs per bucket for drawing a waveform strip.
pub fn waveform_peaks(mono: &[f32], buckets: usize) -> Vec<(f32, f32)> {
    if mono.is_empty() || buckets == 0 {
        return vec![];
    }
    let per = (mono.len() as f64 / buckets as f64).max(1.0);
    (0..buckets)
        .map(|b| {
            let s = (b as f64 * per) as usize;
            let e = (((b + 1) as f64 * per) as usize).min(mono.len());
            if s >= e {
                return (0.0, 0.0);
            }
            mono[s..e]
                .iter()
                .fold((f32::MAX, f32::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)))
        })
        .collect()
}
