use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn ffmpeg_path() -> String {
    std::env::var("TRIO_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// Decode path: hardware backend and whether the downscale happens on the GPU
/// before frames are downloaded (a large win for 4K sources).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwAccel {
    None,
    Vaapi,
    VaapiGpuScale,
    VideoToolbox,
    VideoToolboxGpuScale,
}

impl HwAccel {
    /// Arguments placed before `-i`.
    pub fn input_args(self) -> Vec<&'static str> {
        match self {
            HwAccel::None => vec![],
            HwAccel::Vaapi => vec!["-hwaccel", "vaapi"],
            HwAccel::VaapiGpuScale => vec!["-hwaccel", "vaapi", "-hwaccel_output_format", "vaapi"],
            HwAccel::VideoToolbox => vec!["-hwaccel", "videotoolbox"],
            HwAccel::VideoToolboxGpuScale => vec![
                "-hwaccel",
                "videotoolbox",
                "-hwaccel_output_format",
                "videotoolbox",
            ],
        }
    }

    /// Filter that scales to `w`x`h` and leaves frames in system memory.
    pub fn scale_filter(self, w: u32, h: u32, ten_bit: bool) -> String {
        let fmt = if ten_bit { "p010" } else { "nv12" };
        match self {
            HwAccel::VaapiGpuScale => {
                format!("scale_vaapi=w={w}:h={h}:format={fmt},hwdownload,format={fmt}")
            }
            HwAccel::VideoToolboxGpuScale => {
                format!("scale_vt=w={w}:h={h},hwdownload,format={fmt}")
            }
            _ => format!("scale={w}:{h}:flags=bicubic"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HwAccel::None => "software",
            HwAccel::Vaapi => "VAAPI",
            HwAccel::VaapiGpuScale => "VAAPI + GPU scaling",
            HwAccel::VideoToolbox => "VideoToolbox",
            HwAccel::VideoToolboxGpuScale => "VideoToolbox + GPU scaling",
        }
    }

    fn candidates() -> &'static [HwAccel] {
        if cfg!(target_os = "macos") {
            &[HwAccel::VideoToolboxGpuScale, HwAccel::VideoToolbox]
        } else if cfg!(target_os = "linux") {
            &[HwAccel::VaapiGpuScale, HwAccel::Vaapi]
        } else {
            &[]
        }
    }
}

/// Try each platform decode path on a real clip with the exact filter chain
/// the decoder uses, and keep the first one that produces frames.
pub fn detect_hwaccel(sample: &Path) -> HwAccel {
    for &candidate in HwAccel::candidates() {
        let vf = format!("{},fps=10", candidate.scale_filter(640, 360, false));
        let ok = Command::new(ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
            .args(candidate.input_args())
            .arg("-i")
            .arg(sample)
            .args([
                "-an",
                "-sn",
                "-dn",
                "-vf",
                &vf,
                "-frames:v",
                "2",
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map(|o| {
                o.status.success() && o.stderr.is_empty() && o.stdout.len() == 2 * 640 * 360 * 4
            })
            .unwrap_or(false);
        if ok {
            return candidate;
        }
    }
    HwAccel::None
}

pub fn check_ffmpeg() -> Result<String> {
    let out = Command::new(ffmpeg_path())
        .arg("-version")
        .output()
        .context("ffmpeg not found on PATH (install ffmpeg, or set TRIO_FFMPEG)")?;
    if !out.status.success() {
        return Err(anyhow!("ffmpeg -version failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

/// Fit `src` into `max` keeping aspect, never upscaling, even dimensions.
pub fn fit_size(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (2, 2);
    }
    let s = (max_w as f64 / src_w as f64)
        .min(max_h as f64 / src_h as f64)
        .min(1.0);
    let w = ((src_w as f64 * s).round() as u32).max(2) & !1;
    let h = ((src_h as f64 * s).round() as u32).max(2) & !1;
    (w, h)
}
