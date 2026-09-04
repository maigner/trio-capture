//! Decode one clip to a stream of RGBA frames at a fixed frame rate.
//!
//! `-ss` before `-i` gives a keyframe seek followed by accurate decode to
//! the requested start; the `fps` filter resamples variable frame rate
//! footage onto our fixed timeline. ffmpeg applies rotation metadata itself.

use crate::ffmpeg::{ffmpeg_path, HwAccel};
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct DecodeRequest {
    pub path: PathBuf,
    /// Seconds into the clip.
    pub start: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub hdr: bool,
    pub hwaccel: HwAccel,
}

pub struct Frame {
    /// Seconds into the clip.
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct FrameStream {
    rx: Receiver<Frame>,
    child: Child,
    reader: Option<JoinHandle<()>>,
    pub request: DecodeRequest,
    finished: bool,
}

/// Scale (and tone map HDR) to the requested size, frames in system memory.
fn picture_filters(req: &DecodeRequest) -> Vec<String> {
    let mut filters = vec![req.hwaccel.scale_filter(req.width, req.height, req.hdr)];
    if req.hdr {
        filters.push(
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
             tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
                .to_string(),
        );
    }
    filters
}

/// Decoder command up to the video filter: keyframe seek, then accurate decode.
fn decode_command(req: &DecodeRequest, vf: &str) -> Command {
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(req.hwaccel.input_args())
        .arg("-ss")
        .arg(format!("{:.6}", req.start.max(0.0)))
        .arg("-i")
        .arg(&req.path)
        .args(["-an", "-sn", "-dn", "-vf", vf])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Decode the single frame at `req.start`, for analysis and thumbnails.
pub fn grab_frame(req: &DecodeRequest) -> Result<Frame> {
    let vf = picture_filters(req).join(",");
    let out = decode_command(req, &vf)
        .args(["-frames:v", "1", "-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .output()
        .context("spawning ffmpeg for a frame grab")?;
    let want = (req.width * req.height * 4) as usize;
    if out.stdout.len() < want {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "no frame at {:.2}s in {}: {}",
            req.start,
            req.path.display(),
            err.trim()
        ));
    }
    let mut rgba = out.stdout;
    rgba.truncate(want);
    Ok(Frame {
        time: req.start,
        width: req.width,
        height: req.height,
        rgba,
    })
}

impl FrameStream {
    pub fn start(req: DecodeRequest) -> Result<Self> {
        let mut filters = picture_filters(&req);
        filters.push(format!("fps={}", req.fps));
        let vf = filters.join(",");

        let mut cmd = decode_command(&req, &vf);
        cmd.args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"]);
        let mut child = cmd.spawn().context("spawning ffmpeg decoder")?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let path_for_log = req.path.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tracing::warn!("ffmpeg[{}]: {line}", path_for_log.display());
            }
        });

        let (tx, rx) = bounded::<Frame>(4);
        let frame_len = (req.width * req.height * 4) as usize;
        let (w, h, fps, start) = (req.width, req.height, req.fps, req.start);
        let reader = std::thread::spawn(move || {
            let mut stdout = BufReader::with_capacity(frame_len.min(1 << 22), stdout);
            let mut index: u64 = 0;
            loop {
                let mut buf = vec![0u8; frame_len];
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                let frame = Frame {
                    time: start + index as f64 / fps,
                    width: w,
                    height: h,
                    rgba: buf,
                };
                index += 1;
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            rx,
            child,
            reader: Some(reader),
            request: req,
            finished: false,
        })
    }

    /// Non-blocking.
    pub fn try_next(&mut self) -> Option<Frame> {
        match self.rx.try_recv() {
            Ok(f) => Some(f),
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.finished = true;
                None
            }
            Err(_) => None,
        }
    }

    pub fn next_timeout(&mut self, timeout: Duration) -> Option<Frame> {
        match self.rx.recv_timeout(timeout) {
            Ok(f) => Some(f),
            Err(RecvTimeoutError::Disconnected) => {
                self.finished = true;
                None
            }
            Err(RecvTimeoutError::Timeout) => None,
        }
    }

    /// True once the decoder reached the end of the clip and the queue drained.
    pub fn finished(&self) -> bool {
        self.finished
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        // Release the receiver first: the reader may be blocked on a full
        // channel and would never observe the killed process otherwise.
        let (_tx, empty) = bounded::<Frame>(0);
        drop(std::mem::replace(&mut self.rx, empty));
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}
