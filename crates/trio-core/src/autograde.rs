//! Automatic colour grade: measure each camera on a few frames, then derive
//! one grade per camera so the cameras match each other and sit at a
//! pleasant exposure. The measurements are plain pixel statistics; the
//! decoding of sample frames lives in trio-media.
//!
//! The targets come from the cameras themselves (their average brightness,
//! contrast, colourfulness and colour balance), so a dark gig stays a dark
//! gig and stage lighting keeps its colour. Only brightness has a floor and
//! ceiling so flat or badly exposed footage lands somewhere sensible.

use crate::layout::slot_rects;
use crate::{Grade, Project, CAMERA_COUNT};

/// Frames sampled per camera.
pub const SAMPLE_COUNT: usize = 8;

/// Mean display luma is pulled into this window (0..1, sRGB encoded).
const MEAN_MIN: f32 = 0.28;
const MEAN_MAX: f32 = 0.50;
/// Distance between the 5th and 95th luma percentile after grading.
const SPREAD_MIN: f32 = 0.75;
const SPREAD_MAX: f32 = 0.85;
/// Mean chroma (max - min of sRGB) of the visible pixels after grading.
const CHROMA_MIN: f32 = 0.12;
const CHROMA_MAX: f32 = 0.30;
/// The 90th luma percentile is kept below this after exposure.
const HIGHLIGHT_CEIL: f32 = 0.95;
/// Pixels darker than this are ignored for colour and chroma statistics.
const SHADOW_FLOOR: f32 = 0.08;
/// Pixels brighter than this are ignored for colour statistics (clipped).
const CLIP_FLOOR: f32 = 0.92;

/// How far each camera's colour balance moves toward the cameras' median
/// (1.0 = all the way), applied in log space.
const BALANCE_STRENGTH: f32 = 0.5;

const EXPOSURE_RANGE: (f32, f32) = (-2.5, 2.5);
const CONTRAST_RANGE: (f32, f32) = (0.95, 1.3);
const SATURATION_RANGE: (f32, f32) = (0.8, 1.5);

/// Pixel statistics of one camera, accumulated over its sample frames.
#[derive(Debug, Clone)]
pub struct CameraStats {
    /// Histogram of sRGB luma, 256 bins.
    hist: [u64; 256],
    pixels: u64,
    /// Sum of linear RGB over mid-tone pixels.
    lin_sum: [f64; 3],
    lin_n: u64,
    /// Sum of max - min over visible pixels.
    chroma_sum: f64,
    chroma_n: u64,
    pub frames: usize,
}

impl Default for CameraStats {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraStats {
    pub fn new() -> Self {
        Self {
            hist: [0; 256],
            pixels: 0,
            lin_sum: [0.0; 3],
            lin_n: 0,
            chroma_sum: 0.0,
            chroma_n: 0,
            frames: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pixels == 0
    }

    /// Accumulate one RGBA frame (8 bit, `width * height * 4` bytes).
    pub fn add_frame(&mut self, rgba: &[u8], width: u32, height: u32) {
        self.add_region(rgba, width, height, [0.0, 0.0, 1.0, 1.0]);
    }

    /// Accumulate the part of a frame inside `region` (u0, v0, u1, v1 in
    /// 0..1), which is what the layout actually shows of the camera.
    pub fn add_region(&mut self, rgba: &[u8], width: u32, height: u32, region: [f32; 4]) {
        let (w, h) = (width as usize, height as usize);
        if rgba.len() < w * h * 4 {
            return;
        }
        let x0 = ((region[0] * w as f32).floor() as usize).min(w);
        let x1 = ((region[2] * w as f32).ceil() as usize).clamp(x0, w);
        let y0 = ((region[1] * h as f32).floor() as usize).min(h);
        let y1 = ((region[3] * h as f32).ceil() as usize).clamp(y0, h);
        let mut counted = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y * w + x;
                counted += 1;
                let r = rgba[i * 4] as f32 / 255.0;
                let g = rgba[i * 4 + 1] as f32 / 255.0;
                let b = rgba[i * 4 + 2] as f32 / 255.0;
                let y = luma([r, g, b]);
                self.hist[(y * 255.0).round().clamp(0.0, 255.0) as usize] += 1;
                if y > SHADOW_FLOOR {
                    let max = r.max(g).max(b);
                    let min = r.min(g).min(b);
                    self.chroma_sum += (max - min) as f64;
                    self.chroma_n += 1;
                    if y < CLIP_FLOOR {
                        self.lin_sum[0] += srgb_to_lin(r) as f64;
                        self.lin_sum[1] += srgb_to_lin(g) as f64;
                        self.lin_sum[2] += srgb_to_lin(b) as f64;
                        self.lin_n += 1;
                    }
                }
            }
        }
        self.pixels += counted;
        self.frames += 1;
    }

    /// Key figures for the log.
    pub fn summary(&self) -> String {
        let (rg, bg) = self.balance().unwrap_or((f32::NAN, f32::NAN));
        format!(
            "mean {:.3} p5 {:.3} p95 {:.3} chroma {:.3} R/G {rg:.3} B/G {bg:.3}",
            self.mean_after(0.0),
            self.percentile(0.05),
            self.percentile(0.95),
            self.mean_chroma()
        )
    }

    /// Luma value (0..1) below which `q` (0..1) of the pixels fall.
    fn percentile(&self, q: f32) -> f32 {
        let want = (self.pixels as f64 * q as f64).round() as u64;
        let mut seen = 0;
        for (bin, &n) in self.hist.iter().enumerate() {
            seen += n;
            if seen >= want {
                return bin as f32 / 255.0;
            }
        }
        1.0
    }

    /// Mean display luma after an exposure change of `stops`.
    fn mean_after(&self, stops: f32) -> f32 {
        if self.pixels == 0 {
            return 0.0;
        }
        let k = 2f32.powf(stops);
        let mut sum = 0.0f64;
        for (bin, &n) in self.hist.iter().enumerate() {
            if n > 0 {
                let y = lin_to_srgb(srgb_to_lin(bin as f32 / 255.0) * k);
                sum += y as f64 * n as f64;
            }
        }
        (sum / self.pixels as f64) as f32
    }

    /// Exposure in stops that brings the mean display luma to `target`.
    fn exposure_for_mean(&self, target: f32) -> f32 {
        let (mut lo, mut hi) = (-6.0f32, 6.0f32);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if self.mean_after(mid) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }

    fn mean_chroma(&self) -> f32 {
        if self.chroma_n == 0 {
            0.0
        } else {
            (self.chroma_sum / self.chroma_n as f64) as f32
        }
    }

    /// Linear R/G and B/G of the mid-tones, when enough pixels qualify.
    fn balance(&self) -> Option<(f32, f32)> {
        if self.lin_n < 64 {
            return None;
        }
        let [r, g, b] = self.lin_sum;
        if g <= 0.0 {
            return None;
        }
        Some(((r / g) as f32, (b / g) as f32))
    }
}

/// One grade per entry of `stats`; cameras without pixels get the default.
pub fn solve(stats: &[CameraStats]) -> Vec<Grade> {
    let mut grades = vec![Grade::default(); stats.len()];
    let live: Vec<usize> = (0..stats.len()).filter(|&i| !stats[i].is_empty()).collect();
    if live.is_empty() {
        return grades;
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;

    // Brightness target: what the cameras average, kept in a sensible window.
    let means: Vec<f32> = live.iter().map(|&i| stats[i].mean_after(0.0)).collect();
    let target_mean = mean(&means).clamp(MEAN_MIN, MEAN_MAX);

    // Contrast from the luma spread after a first exposure guess.
    let spreads: Vec<f32> = live
        .iter()
        .map(|&i| {
            let s = &stats[i];
            let e = s.exposure_for_mean(target_mean);
            shift(s.percentile(0.95), e) - shift(s.percentile(0.05), e)
        })
        .collect();
    let target_spread = mean(&spreads).clamp(SPREAD_MIN, SPREAD_MAX);
    let contrasts: Vec<f32> = spreads
        .iter()
        .map(|&sp| (target_spread / sp.max(0.05)).clamp(CONTRAST_RANGE.0, CONTRAST_RANGE.1))
        .collect();

    // Exposure so the mean lands on target once contrast has pivoted it.
    let exposures: Vec<f32> = live
        .iter()
        .zip(&contrasts)
        .map(|(&i, &c)| {
            let s = &stats[i];
            let pre = (target_mean - 0.5) / c + 0.5;
            let e = s.exposure_for_mean(pre);
            let p90 = s.percentile(0.90).max(1.0 / 255.0);
            let guard = (srgb_to_lin(HIGHLIGHT_CEIL) / srgb_to_lin(p90)).log2();
            e.min(guard.max(0.0))
                .clamp(EXPOSURE_RANGE.0, EXPOSURE_RANGE.1)
        })
        .collect();

    // Colourfulness after exposure and contrast, matched across cameras.
    let chromas: Vec<f32> = live
        .iter()
        .zip(exposures.iter().zip(&contrasts))
        .map(|(&i, (&e, &c))| {
            let s = &stats[i];
            let m0 = s.mean_after(0.0).max(1.0 / 255.0);
            s.mean_chroma() * (s.mean_after(e) / m0) * c
        })
        .collect();
    let target_chroma = mean(&chromas).clamp(CHROMA_MIN, CHROMA_MAX);
    let saturations: Vec<f32> = chromas
        .iter()
        .map(|&ch| (target_chroma / ch.max(0.01)).clamp(SATURATION_RANGE.0, SATURATION_RANGE.1))
        .collect();

    // Colour balance: pull every camera halfway to the median of the cameras.
    // Cameras often look at differently lit parts of the stage, so a full
    // match would wash the real colour out of one of them. With a single
    // camera there is nothing to match, and neutral is not a safe guess
    // under stage lighting.
    let balances: Vec<Option<(f32, f32)>> = live.iter().map(|&i| stats[i].balance()).collect();
    let known: Vec<(f32, f32)> = balances.iter().flatten().copied().collect();
    let target_balance = if known.len() >= 2 {
        let mut rg: Vec<f32> = known.iter().map(|b| b.0).collect();
        let mut bg: Vec<f32> = known.iter().map(|b| b.1).collect();
        Some((median(&mut rg), median(&mut bg)))
    } else {
        None
    };

    for (k, &i) in live.iter().enumerate() {
        let g = &mut grades[i];
        g.exposure = round2(exposures[k]);
        g.contrast = round2(contrasts[k]);
        g.saturation = round2(saturations[k]);
        if let (Some((r_star, b_star)), Some((rg, bg))) = (target_balance, balances[k]) {
            let a = (r_star / rg.max(1e-3)).powf(BALANCE_STRENGTH);
            let c = (b_star / bg.max(1e-3)).powf(BALANCE_STRENGTH);
            let (t, tint) = balance_to_grade(a, c);
            g.temperature = round2(t.clamp(-1.0, 1.0));
            g.tint = round2(tint.clamp(-1.0, 1.0));
        }
    }
    grades
}

/// Temperature and tint whose channel gains (see `shader.wgsl`) scale R by
/// `a` and B by `c` relative to G.
fn balance_to_grade(a: f32, c: f32) -> (f32, f32) {
    let sum = (a + c).max(1e-3);
    let temperature = 4.0 * (a - c) / sum;
    let tint = 4.0 * (1.0 - 2.0 / sum);
    (temperature, tint)
}

/// Display value `y` after an exposure change of `stops`.
fn shift(y: f32, stops: f32) -> f32 {
    lin_to_srgb(srgb_to_lin(y) * 2f32.powf(stops))
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// Master times at which to sample frames: spread over the export range
/// (or the whole timeline), preferring moments that all cameras cover.
pub fn sample_times(project: &Project, count: usize) -> Vec<f64> {
    let (start, end) = if project.range.end > project.range.start {
        (project.range.start, project.range.end)
    } else {
        (0.0, project.duration(None))
    };
    if end <= start || count == 0 {
        return Vec::new();
    }
    let grid = 200;
    let margin = (end - start) * 0.02;
    let candidates: Vec<(f64, usize)> = (0..grid)
        .map(|k| {
            let t = start + margin + (end - start - 2.0 * margin) * k as f64 / (grid - 1) as f64;
            let cover = (0..CAMERA_COUNT)
                .filter(|&cam| project.clip_at(cam, t).is_some())
                .count();
            (t, cover)
        })
        .collect();
    let best = candidates.iter().map(|c| c.1).max().unwrap_or(0);
    if best == 0 {
        return Vec::new();
    }
    let good: Vec<f64> = candidates
        .into_iter()
        .filter(|c| c.1 == best)
        .map(|c| c.0)
        .collect();
    let count = count.min(good.len());
    (0..count)
        .map(|k| match count {
            1 => good[good.len() / 2],
            _ => good[k * (good.len() - 1) / (count - 1)],
        })
        .collect()
}

/// Part of the source frame (u0, v0, u1, v1) the layout shows of `cam`:
/// cover fit into its slot, then zoom and pan, exactly as the shader does.
/// The whole frame when the camera occupies no slot.
pub fn visible_region(project: &Project, cam: usize, src_w: u32, src_h: u32) -> [f32; 4] {
    let rects = slot_rects(project.layout);
    let (out_w, out_h) = project.output_size();
    let Some((slot, rect)) = project
        .slots
        .iter()
        .zip(rects.iter())
        .find(|(s, _)| s.camera == cam)
    else {
        return [0.0, 0.0, 1.0, 1.0];
    };
    let slot_aspect = (rect.w * out_w as f32) / (rect.h * out_h as f32).max(1.0);
    let src_aspect = src_w as f32 / (src_h as f32).max(1.0);
    let mut region = [1.0f32, 1.0f32];
    if src_aspect > slot_aspect {
        region[0] = slot_aspect / src_aspect;
    } else {
        region[1] = src_aspect / slot_aspect;
    }
    let zoom = slot.zoom.max(0.01);
    region = [region[0] / zoom, region[1] / zoom];
    let center = [
        (0.5 + slot.pan[0]).clamp(region[0] * 0.5, 1.0 - region[0] * 0.5),
        (0.5 + slot.pan[1]).clamp(region[1] * 0.5, 1.0 - region[1] * 0.5),
    ];
    [
        (center[0] - region[0] * 0.5).max(0.0),
        (center[1] - region[1] * 0.5).max(0.0),
        (center[0] + region[0] * 0.5).min(1.0),
        (center[1] + region[1] * 0.5).min(1.0),
    ]
}

pub fn luma(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

pub fn srgb_to_lin(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c.max(0.0) + 0.055) / 1.055).powf(2.4)
    }
}

pub fn lin_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

impl Grade {
    /// CPU mirror of the shader's grade, sRGB in and out (0..1).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let k = 2f32.powf(self.exposure);
        let gains = [
            1.0 + 0.25 * self.temperature,
            1.0 - 0.25 * self.tint,
            1.0 - 0.25 * self.temperature,
        ];
        let mut d = [0.0f32; 3];
        for i in 0..3 {
            let c = srgb_to_lin(rgb[i]) * k * gains[i];
            let v = lin_to_srgb(c);
            let v = (v * self.gain + self.lift)
                .max(0.0)
                .powf(1.0 / self.gamma.max(0.05));
            d[i] = (v - 0.5) * self.contrast + 0.5;
        }
        let l = luma(d);
        let mut out = [0.0f32; 3];
        for i in 0..3 {
            out[i] = (l + (d[i] - l) * self.saturation).clamp(0.0, 1.0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64x64 frame with a luma ramp so the percentiles are spread out.
    fn frame(scale: f32, cast: [f32; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64 {
            for x in 0..64 {
                let v = scale * (0.1 + 0.9 * (x as f32 / 63.0)) * (0.5 + 0.5 * (y as f32 / 63.0));
                for c in cast {
                    out.push(((v * c).clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                out.push(255);
            }
        }
        out
    }

    fn stats(scale: f32, cast: [f32; 3]) -> CameraStats {
        let mut s = CameraStats::new();
        s.add_frame(&frame(scale, cast), 64, 64);
        s
    }

    fn graded_mean(scale: f32, cast: [f32; 3], g: &Grade) -> ([f32; 3], f32) {
        let f = frame(scale, cast);
        let mut sum = [0.0f32; 3];
        let mut lum = 0.0;
        for px in f.chunks(4) {
            let out = g.apply([
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ]);
            for i in 0..3 {
                sum[i] += out[i];
            }
            lum += luma(out);
        }
        let n = (f.len() / 4) as f32;
        ([sum[0] / n, sum[1] / n, sum[2] / n], lum / n)
    }

    #[test]
    fn dark_and_bright_cameras_meet() {
        let s = [
            stats(0.35, [1.0; 3]),
            stats(1.0, [1.0; 3]),
            CameraStats::new(),
        ];
        let g = solve(&s);
        assert!(g[0].exposure > 0.3, "dark camera lifted: {:?}", g[0]);
        assert!(g[1].exposure < -0.3, "bright camera lowered: {:?}", g[1]);
        assert_eq!(g[2], Grade::default());
        let (_, m0) = graded_mean(0.35, [1.0; 3], &g[0]);
        let (_, m1) = graded_mean(1.0, [1.0; 3], &g[1]);
        assert!((m0 - m1).abs() < 0.04, "means {m0} vs {m1}");
        assert!(m0 > MEAN_MIN - 0.03 && m0 < MEAN_MAX + 0.03, "mean {m0}");
    }

    #[test]
    fn warm_and_cool_cameras_match() {
        let warm = [1.15, 1.0, 0.85];
        let cool = [0.88, 1.0, 1.12];
        let s = [stats(0.6, warm), stats(0.6, cool)];
        let g = solve(&s);
        assert!(g[0].temperature < -0.1, "warm camera cooled: {:?}", g[0]);
        assert!(g[1].temperature > 0.1, "cool camera warmed: {:?}", g[1]);
        let ratio = |c: [f32; 3]| (c[0] / c[1], c[2] / c[1]);
        let gap = |ga: &Grade, gb: &Grade| {
            let (a, _) = graded_mean(0.6, warm, ga);
            let (b, _) = graded_mean(0.6, cool, gb);
            let (ra, ba) = ratio(a);
            let (rb, bb) = ratio(b);
            ((ra - rb).abs(), (ba - bb).abs())
        };
        let before = gap(&Grade::default(), &Grade::default());
        let after = gap(&g[0], &g[1]);
        // Half strength in linear light: the display gap shrinks clearly but
        // does not vanish.
        assert!(after.0 < 0.8 * before.0, "R/G gap {before:?} -> {after:?}");
        assert!(after.1 < 0.8 * before.1, "B/G gap {before:?} -> {after:?}");
        assert!(after.0 > 0.3 * before.0, "R/G gap {before:?} -> {after:?}");
    }

    #[test]
    fn well_exposed_single_camera_stays_close_to_neutral() {
        let s = [stats(0.8, [1.0; 3])];
        let g = solve(&s)[0];
        assert!(g.exposure.abs() < 0.35, "{g:?}");
        assert_eq!(g.temperature, 0.0);
        assert_eq!(g.tint, 0.0);
        assert!(g.contrast >= 0.9 && g.contrast <= 1.3);
    }

    #[test]
    fn balance_gains_round_trip() {
        let (t, tint) = balance_to_grade(1.1, 0.9);
        let rg = 1.0 + 0.25 * t;
        let gg = 1.0 - 0.25 * tint;
        let bg = 1.0 - 0.25 * t;
        assert!((rg / gg - 1.1).abs() < 1e-4);
        assert!((bg / gg - 0.9).abs() < 1e-4);
    }

    #[test]
    fn visible_region_follows_cover_fit_and_zoom() {
        let mut p = Project::default();
        p.layout = crate::LayoutId::VThree;
        // 1080x1920 output, each slot 1080x640 (1.69:1): a 16:9 source loses
        // a little width and keeps its full height.
        let r = visible_region(&p, 0, 1920, 1080);
        assert!(
            (r[1] - 0.0).abs() < 1e-4 && (r[3] - 1.0).abs() < 1e-4,
            "{r:?}"
        );
        let want = (1080.0 / 640.0) / (1920.0 / 1080.0);
        assert!((r[2] - r[0] - want).abs() < 1e-3, "{r:?}");
        p.slots[0].zoom = 2.0;
        p.slots[0].pan = [0.5, 0.0];
        let z = visible_region(&p, 0, 1920, 1080);
        assert!((z[2] - z[0] - want / 2.0).abs() < 1e-4, "{z:?}");
        assert!((z[2] - 1.0).abs() < 1e-4, "pan clamps at the edge: {z:?}");
        // A camera in no slot is measured whole.
        p.slots[2].camera = 0;
        assert_eq!(visible_region(&p, 2, 1920, 1080), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn sample_times_prefer_full_coverage() {
        use crate::Clip;
        let mut p = Project::default();
        let clip = |offset: f64, duration: f64| Clip {
            path: "x.mp4".into(),
            duration,
            width: 16,
            height: 9,
            fps: 30.0,
            rotation: 0,
            hdr: false,
            has_audio: true,
            creation_time: None,
            offset,
            sync_confidence: None,
        };
        p.cameras[0].clips = vec![clip(0.0, 100.0)];
        p.cameras[1].clips = vec![clip(40.0, 30.0)];
        p.cameras[2].clips = vec![clip(20.0, 80.0)];
        let times = sample_times(&p, 5);
        assert_eq!(times.len(), 5);
        for t in &times {
            assert!(*t >= 40.0 && *t < 70.0, "t={t}");
        }
        assert!(times.windows(2).all(|w| w[1] > w[0]));
    }
}
