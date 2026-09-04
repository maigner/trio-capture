//! Feed composed RGBA frames into an ffmpeg encoder process.

use crate::ffmpeg::ffmpeg_path;
use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use trio_core::Codec;

#[derive(Debug, Clone)]
pub struct ExportSpec {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: Codec,
    pub quality: u32,
    pub out_path: PathBuf,
    pub wav: Option<PathBuf>,
    /// Seconds into the WAV where the export starts.
    pub audio_start: f64,
    pub duration: f64,
}

pub struct Encoder {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
    frame_len: usize,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Encoder {
    pub fn start(spec: &ExportSpec) -> Result<Self> {
        let mut cmd = Command::new(ffmpeg_path());
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin"]);
        if matches!(spec.codec, Codec::H264Vaapi | Codec::H265Vaapi) {
            cmd.args(["-vaapi_device", "/dev/dri/renderD128"]);
        }
        cmd.args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .arg("-s")
            .arg(format!("{}x{}", spec.width, spec.height))
            .arg("-r")
            .arg(format!("{}", spec.fps))
            .args(["-i", "-"]);
        if let Some(wav) = &spec.wav {
            cmd.arg("-ss")
                .arg(format!("{:.6}", spec.audio_start.max(0.0)))
                .arg("-t")
                .arg(format!("{:.6}", spec.duration))
                .arg("-i")
                .arg(wav);
        }
        cmd.args(["-map", "0:v"]);
        if spec.wav.is_some() {
            cmd.args(["-map", "1:a", "-c:a", "aac", "-b:a", "256k"]);
        }
        let q = spec.quality.to_string();
        let mbps = bitrate_mbps(spec.width, spec.height, spec.fps);
        match spec.codec {
            Codec::H264Software => {
                cmd.args([
                    "-c:v", "libx264", "-preset", "medium", "-crf", &q, "-pix_fmt", "yuv420p",
                ]);
            }
            Codec::H265Software => {
                cmd.args([
                    "-c:v", "libx265", "-preset", "medium", "-crf", &q, "-pix_fmt", "yuv420p",
                    "-tag:v", "hvc1",
                ]);
            }
            Codec::H264Vaapi => {
                cmd.args([
                    "-vf",
                    "format=nv12,hwupload",
                    "-c:v",
                    "h264_vaapi",
                    "-qp",
                    &q,
                ]);
            }
            Codec::H265Vaapi => {
                cmd.args([
                    "-vf",
                    "format=nv12,hwupload",
                    "-c:v",
                    "hevc_vaapi",
                    "-qp",
                    &q,
                    "-tag:v",
                    "hvc1",
                ]);
            }
            Codec::H264VideoToolbox => {
                cmd.args([
                    "-c:v",
                    "h264_videotoolbox",
                    "-b:v",
                    &format!("{mbps}M"),
                    "-pix_fmt",
                    "yuv420p",
                ]);
            }
            Codec::H265VideoToolbox => {
                cmd.args([
                    "-c:v",
                    "hevc_videotoolbox",
                    "-b:v",
                    &format!("{mbps}M"),
                    "-pix_fmt",
                    "yuv420p",
                    "-tag:v",
                    "hvc1",
                ]);
            }
        }
        cmd.args(["-movflags", "+faststart", "-shortest"])
            .arg(&spec.out_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().context("spawning ffmpeg encoder")?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tracing::warn!("ffmpeg[encode]: {line}");
                log2.lock().unwrap().push(line);
            }
        });
        Ok(Self {
            child,
            stdin: Some(BufWriter::with_capacity(1 << 22, stdin)),
            frame_len: (spec.width * spec.height * 4) as usize,
            log,
        })
    }

    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<()> {
        if rgba.len() != self.frame_len {
            return Err(anyhow!(
                "frame size {} != expected {}",
                rgba.len(),
                self.frame_len
            ));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("encoder closed"))?;
        stdin
            .write_all(rgba)
            .map_err(|e| anyhow!("encoder rejected frame: {e}. {}", self.errors()))
    }

    pub fn finish(mut self) -> Result<()> {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.flush();
        }
        let status = self.child.wait().context("waiting for encoder")?;
        if !status.success() {
            return Err(anyhow!("encoder exited with {status}: {}", self.errors()));
        }
        Ok(())
    }

    fn errors(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bitrate_mbps(w: u32, h: u32, fps: f64) -> u32 {
    let px = (w * h) as f64 * fps.max(1.0);
    ((px / (1920.0 * 1080.0 * 30.0)) * 16.0)
        .clamp(4.0, 80.0)
        .round() as u32
}
