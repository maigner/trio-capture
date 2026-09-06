//! Find where the people are in a camera's picture. Musicians move and the
//! stage does not, so the difference between a few frames taken a moment
//! apart outlines the band. Skin tones narrow that down to faces and hands
//! when the light is white enough for them to mean anything; under warm
//! stage light everything looks like skin, and the heads are then taken to
//! be the top of what moves. One box pair per sample moment, spread over
//! the whole timeline, so the automatic movement can follow a singer who
//! walks about. Only pixel statistics live here; decoding the sample frames
//! is trio-media's job.

use crate::{Subject, SubjectSample};

/// Frames per sample moment.
pub const FRAMES_PER_SAMPLE: usize = 3;
/// Seconds between the frames of a sample.
pub const FRAME_GAP: f64 = 0.5;
/// Seconds between sample moments, unless the timeline is very long.
pub const SAMPLE_STEP: f64 = 12.0;
/// Sample moments per camera at most.
pub const MAX_SAMPLES: usize = 300;

/// Cells per side of the accumulation grid (normalised, aspect ignored).
pub const GRID: usize = 64;
/// Luma difference (0..1) below which a pixel counts as still.
const MOTION_FLOOR: f32 = 0.04;
/// Luma difference at which a pixel counts as fully moving.
const MOTION_CEIL: f32 = 0.25;
/// Mean motion above this means the whole picture changed (a camera bump,
/// a lighting flash); such a sample is ignored.
const SHAKE_LIMIT: f32 = 0.35;
/// Cells moving at least this much are clearly something, not noise.
const STRONG_CELL: f32 = 0.2;
/// Clearly moving cells needed to trust a sample.
const MIN_STRONG_CELLS: usize = 3;
/// Radius (in cells) of the neighbourhood a cell's movement is averaged
/// over before it is judged: people are dense blobs of moving cells,
/// noise and reflections are scattered specks.
const DENSITY_RADIUS: usize = 2;
/// Neighbourhoods moving at least this share of the 99th percentile
/// neighbourhood are surely people (or something equally big).
const SEED_SHARE: f32 = 0.3;
/// Neighbourhoods moving at least this share count too, but only when
/// they touch a sure region: a nodding head next to strumming hands stays,
/// scattered specks on the ceiling do not.
const KEEP_SHARE: f32 = 0.08;
/// Share of the people box height added above it for the heads when no
/// skin can be seen, in case the heads moved too little to be caught.
const HEAD_ABOVE: f32 = 0.15;
/// Cells below this are always noise.
const CELL_FLOOR: f32 = 0.06;
/// Moving cells covering more of the frame than this mean the camera
/// moved, not the people; such a sample is ignored.
const SPREAD_LIMIT: f32 = 0.35;
/// Skin covering more of the frame than this means the light, not the
/// people, is skin-coloured.
const SKIN_COVER_LIMIT: f32 = 0.2;
/// Skin-motion mass (in whole cells) needed to trust the face box.
const MIN_FACE_MASS: f32 = 0.5;
/// Share of the people box height, from the top, taken as the heads.
const HEAD_SHARE: f32 = 0.4;
/// Share of the mass left outside the people box on each side.
const PEOPLE_TAIL: f32 = 0.03;
/// Share of the mass left outside the face box on each side.
const FACE_TAIL: f32 = 0.05;

/// Sample moments over the whole timeline of `duration` seconds.
pub fn sample_times(duration: f64) -> Vec<f64> {
    if duration <= 1.0 {
        return Vec::new();
    }
    let step = SAMPLE_STEP.max(duration / MAX_SAMPLES as f64);
    let count = ((duration / step).floor() as usize).max(1);
    (0..count)
        .map(|k| step * 0.5 + step * k as f64)
        .filter(|&t| t < duration)
        .collect()
}

/// Soft membership of `v` in `lo..=hi`, fading to 0 over `soft` outside.
fn inside(v: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    ((v - lo).min(hi - v) / soft + 1.0).clamp(0.0, 1.0)
}

/// How much an RGB pixel looks like skin (0..1): the classic YCbCr window
/// that holds for most skin tones under white-ish light.
pub fn skin(r: u8, g: u8, b: u8) -> f32 {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = 128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b;
    let cr = 128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    inside(cb, 77.0, 127.0, 4.0)
        * inside(cr, 133.0, 173.0, 4.0)
        * inside(cr - cb, 15.0, 120.0, 5.0)
        * inside(y, 40.0, 245.0, 15.0)
}

fn luma(p: &[u8]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Saturation (0..1) above which a bright pixel is a coloured light (a neon
/// sign, an LED rope) rather than anything lit by one.
const LIGHT_SATURATION: f32 = 0.8;
/// Luma (0..1) a pixel needs on top of that saturation to count as a light.
const LIGHT_LUMA: f32 = 0.4;

/// A vividly coloured bright pixel: a light source, whose chasing and
/// blinking must not read as movement.
fn is_light(p: &[u8]) -> bool {
    let max = p[0].max(p[1]).max(p[2]) as f32 / 255.0;
    let min = p[0].min(p[1]).min(p[2]) as f32 / 255.0;
    max > 0.0 && (max - min) / max > LIGHT_SATURATION && luma(p) > LIGHT_LUMA
}

/// Luma of every 2x2 block of an RGBA frame, half the size of the frame,
/// and which blocks hold a light.
fn block_luma(rgba: &[u8], w: usize, h: usize) -> (Vec<f32>, Vec<bool>) {
    let (bw, bh) = (w / 2, h / 2);
    let mut out = vec![0.0f32; bw * bh];
    let mut lights = vec![false; bw * bh];
    for by in 0..bh {
        for bx in 0..bw {
            let mut l = 0.0;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = ((by * 2 + dy) * w + bx * 2 + dx) * 4;
                l += luma(&rgba[i..i + 4]);
                lights[by * bw + bx] |= is_light(&rgba[i..i + 4]);
            }
            out[by * bw + bx] = l * 0.25;
        }
    }
    (out, lights)
}

/// Side of the tiles (in block luma pixels) within which brightness is
/// equalised between the frames. A flickering light or drifting exposure
/// changes a tile's brightness without moving anything, so it cancels;
/// a person moving shifts texture inside the tile, which does not.
const TILE: usize = 8;

/// Scale every tile of `other` so its mean matches the same tile of `base`.
fn equalise_tiles(base: &[f32], other: &mut [f32], bw: usize, bh: usize) {
    for ty in (0..bh).step_by(TILE) {
        for tx in (0..bw).step_by(TILE) {
            let (mut sb, mut so) = (0.0f32, 0.0f32);
            for y in ty..(ty + TILE).min(bh) {
                for x in tx..(tx + TILE).min(bw) {
                    sb += base[y * bw + x];
                    so += other[y * bw + x];
                }
            }
            if so > 1e-4 {
                let k = (sb / so).clamp(0.5, 2.0);
                for y in ty..(ty + TILE).min(bh) {
                    for x in tx..(tx + TILE).min(bw) {
                        other[y * bw + x] *= k;
                    }
                }
            }
        }
    }
}

/// Mean movement per grid cell (0..1) between a few RGBA frames of the
/// same size, or `None` when the whole picture changed.
pub fn motion_grid(frames: &[&[u8]], width: u32, height: u32) -> Option<Vec<f32>> {
    let (w, h) = (width as usize, height as usize);
    let (bw, bh) = (w / 2, h / 2);
    if bw == 0 || bh == 0 || frames.len() < 2 || frames.iter().any(|f| f.len() < w * h * 4) {
        return None;
    }
    let (first, mut lights) = block_luma(frames[0], w, h);
    let mut lumas = vec![first];
    for f in &frames[1..] {
        let (mut l, lit) = block_luma(f, w, h);
        equalise_tiles(&lumas[0], &mut l, bw, bh);
        lumas.push(l);
        for (a, b) in lights.iter_mut().zip(lit) {
            *a |= b;
        }
    }
    let mut motion = vec![0.0f32; GRID * GRID];
    let mut count = vec![0u32; GRID * GRID];
    let mut total = 0.0f32;
    for by in 0..bh {
        let cy = by * GRID / bh;
        for bx in 0..bw {
            let i = by * bw + bx;
            if lights[i] {
                continue;
            }
            let mut diff = 0.0f32;
            for a in 0..lumas.len() {
                for b in a + 1..lumas.len() {
                    diff = diff.max((lumas[a][i] - lumas[b][i]).abs());
                }
            }
            let m = ((diff - MOTION_FLOOR) / (MOTION_CEIL - MOTION_FLOOR)).clamp(0.0, 1.0);
            let c = cy * GRID + bx * GRID / bw;
            motion[c] += m;
            count[c] += 1;
            total += m;
        }
    }
    if total / (bw * bh) as f32 > SHAKE_LIMIT {
        return None;
    }
    for c in 0..GRID * GRID {
        if count[c] > 0 {
            motion[c] /= count[c] as f32;
        }
    }
    Some(motion)
}

/// Mean skin likelihood per grid cell (0..1) over the frames, and the share
/// of the whole frame that looks like skin.
fn skin_grid(frames: &[&[u8]], width: u32, height: u32) -> (Vec<f32>, f32) {
    let (w, h) = (width as usize, height as usize);
    let mut cells = vec![0.0f32; GRID * GRID];
    let mut count = vec![0u32; GRID * GRID];
    let mut total = 0.0f32;
    for y in 0..h {
        let cy = y * GRID / h;
        for x in 0..w {
            let i = (y * w + x) * 4;
            let s = frames
                .iter()
                .map(|f| skin(f[i], f[i + 1], f[i + 2]))
                .fold(0.0f32, f32::max);
            let c = cy * GRID + x * GRID / w;
            cells[c] += s;
            count[c] += 1;
            total += s;
        }
    }
    for c in 0..GRID * GRID {
        if count[c] > 0 {
            cells[c] /= count[c] as f32;
        }
    }
    (cells, total / (w * h).max(1) as f32)
}

/// Where the people are in a few RGBA frames of the same size taken a
/// moment apart, or `None` when too little moved to tell.
pub fn analyse(frames: &[&[u8]], width: u32, height: u32) -> Option<([f32; 4], [f32; 4])> {
    let mut motion = motion_grid(frames, width, height)?;
    let strong = motion.iter().filter(|&&m| m >= STRONG_CELL).count();
    if strong < MIN_STRONG_CELLS {
        return None;
    }
    let density = blur(&motion, DENSITY_RADIUS);
    let mut sorted = density.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p99 = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)];
    let seed = CELL_FLOOR.max(SEED_SHARE * p99);
    let keep = CELL_FLOOR.max(KEEP_SHARE * p99);
    let kept = grow_from_seeds(&density, seed, keep);
    for (m, k) in motion.iter_mut().zip(&kept) {
        if !k {
            *m = 0.0;
        }
    }
    let moving = motion.iter().filter(|&&m| m > 0.0).count();
    if moving as f32 > SPREAD_LIMIT * (GRID * GRID) as f32 {
        return None;
    }
    let people = mass_box(&motion, PEOPLE_TAIL);
    let (skin_cells, skin_cover) = skin_grid(frames, width, height);
    let faces_mass: Vec<f32> = motion.iter().zip(&skin_cells).map(|(m, s)| m * s).collect();
    let faces = if skin_cover < SKIN_COVER_LIMIT && faces_mass.iter().sum::<f32>() >= MIN_FACE_MASS
    {
        mass_box(&faces_mass, FACE_TAIL)
    } else {
        let height = people[3] - people[1];
        [
            people[0],
            (people[1] - height * HEAD_ABOVE).max(0.0),
            people[2],
            people[1] + height * HEAD_SHARE,
        ]
    };
    Some((faces, people))
}

/// Mean over the square neighbourhood of `radius` cells around each cell.
fn blur(cells: &[f32], radius: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; GRID * GRID];
    for y in 0..GRID {
        for x in 0..GRID {
            let (y0, y1) = (y.saturating_sub(radius), (y + radius).min(GRID - 1));
            let (x0, x1) = (x.saturating_sub(radius), (x + radius).min(GRID - 1));
            let mut sum = 0.0;
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    sum += cells[yy * GRID + xx];
                }
            }
            out[y * GRID + x] = sum / ((y1 - y0 + 1) * (x1 - x0 + 1)) as f32;
        }
    }
    out
}

/// Cells at or above `seed`, plus every cell at or above `keep` that is
/// connected to one of them (8-neighbourhood).
fn grow_from_seeds(cells: &[f32], seed: f32, keep: f32) -> Vec<bool> {
    let mut kept = vec![false; GRID * GRID];
    let mut stack: Vec<usize> = (0..GRID * GRID).filter(|&c| cells[c] >= seed).collect();
    for &c in &stack {
        kept[c] = true;
    }
    while let Some(c) = stack.pop() {
        let (x, y) = (c % GRID, c / GRID);
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= GRID as i32 || ny >= GRID as i32 {
                    continue;
                }
                let n = ny as usize * GRID + nx as usize;
                if !kept[n] && cells[n] >= keep {
                    kept[n] = true;
                    stack.push(n);
                }
            }
        }
    }
    kept
}

/// The box holding all but `tail` of the mass on each side of both axes.
fn mass_box(cells: &[f32], tail: f32) -> [f32; 4] {
    let mut cols = vec![0.0f32; GRID];
    let mut rows = vec![0.0f32; GRID];
    for (c, &v) in cells.iter().enumerate() {
        cols[c % GRID] += v;
        rows[c / GRID] += v;
    }
    let (x0, x1) = span(&cols, tail);
    let (y0, y1) = span(&rows, tail);
    [x0, y0, x1, y1]
}

/// First and last cell edge (0..1) between which all but `tail` of the
/// mass lies on each side.
fn span(marginal: &[f32], tail: f32) -> (f32, f32) {
    let total: f32 = marginal.iter().sum();
    if total <= 0.0 {
        return (0.0, 1.0);
    }
    let n = marginal.len();
    let mut acc = 0.0;
    let mut lo = 0;
    for (i, &v) in marginal.iter().enumerate() {
        acc += v;
        if acc >= total * tail {
            lo = i;
            break;
        }
    }
    acc = 0.0;
    let mut hi = n - 1;
    for (i, &v) in marginal.iter().enumerate().rev() {
        acc += v;
        if acc >= total * tail {
            hi = i;
            break;
        }
    }
    let hi = hi.max(lo);
    (lo as f32 / n as f32, (hi + 1) as f32 / n as f32)
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn lerp(a: [f32; 4], b: [f32; 4], f: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for i in 0..4 {
        out[i] = a[i] + (b[i] - a[i]) * f;
    }
    out
}

impl Subject {
    /// The boxes at master time `t`: neighbouring samples are joined so a
    /// walking singer stays inside, and the result changes smoothly.
    pub fn at(&self, t: f64) -> Option<SubjectSample> {
        let s = &self.samples;
        if s.is_empty() {
            return None;
        }
        let window = |k: usize| {
            let lo = k.saturating_sub(1);
            let hi = (k + 1).min(s.len() - 1);
            let mut faces = s[k].faces;
            let mut people = s[k].people;
            for x in &s[lo..=hi] {
                faces = union(faces, x.faces);
                people = union(people, x.people);
            }
            (faces, people)
        };
        let k = s.partition_point(|x| x.t <= t);
        let (faces, people) = if k == 0 {
            window(0)
        } else if k >= s.len() {
            window(s.len() - 1)
        } else {
            let (a, b) = (&s[k - 1], &s[k]);
            let f = ((t - a.t) / (b.t - a.t).max(1e-6)).clamp(0.0, 1.0) as f32;
            let (fa, pa) = window(k - 1);
            let (fb, pb) = window(k);
            (lerp(fa, fb, f), lerp(pa, pb, f))
        };
        Some(SubjectSample { t, faces, people })
    }

    /// The box around every face ever seen, for a quick summary.
    pub fn faces_overall(&self) -> Option<[f32; 4]> {
        self.samples.iter().map(|s| s.faces).reduce(union)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grey picture with a skin-coloured square at `pos`.
    fn frame(w: usize, h: usize, pos: (usize, usize), size: usize) -> Vec<u8> {
        let mut f = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let inside = x >= pos.0 && x < pos.0 + size && y >= pos.1 && y < pos.1 + size;
                let px = if inside {
                    [225, 170, 140, 255]
                } else {
                    [40, 40, 45, 255]
                };
                f[i..i + 4].copy_from_slice(&px);
            }
        }
        f
    }

    #[test]
    fn skin_window() {
        assert!(skin(225, 170, 140) > 0.9);
        assert!(skin(40, 40, 45) < 0.1);
        assert!(skin(30, 80, 220) < 0.1);
        assert!(skin(120, 90, 80) > 0.9, "dim skin");
        assert!(skin(128, 128, 128) < 0.05, "grey");
    }

    #[test]
    fn finds_a_moving_face() {
        let (w, h) = (128, 72);
        let a = frame(w, h, (80, 20), 16);
        let b = frame(w, h, (84, 22), 16);
        let c = frame(w, h, (82, 24), 16);
        let (faces, people) = analyse(&[&a, &b, &c], w as u32, h as u32).expect("subject");
        assert!(faces[0] > 0.55 && faces[2] < 0.85, "{faces:?}");
        assert!(faces[1] > 0.2 && faces[3] < 0.6, "{faces:?}");
        assert!(people[0] <= faces[0] && people[2] >= faces[2], "{people:?}");
    }

    #[test]
    fn a_still_picture_has_no_subject() {
        let (w, h) = (128, 72);
        let f = frame(w, h, (80, 20), 16);
        assert!(analyse(&[&f, &f, &f], w as u32, h as u32).is_none());
    }

    #[test]
    fn a_flash_is_ignored() {
        let (w, h) = (128, 72);
        let dark = frame(w, h, (0, 0), 0);
        let bright = vec![200u8; w * h * 4];
        assert!(analyse(&[&dark, &bright], w as u32, h as u32).is_none());
    }

    #[test]
    fn samples_cover_the_timeline() {
        assert!(sample_times(0.5).is_empty());
        let t = sample_times(100.0);
        assert_eq!(t.len(), 8);
        assert!(t[0] > 0.0 && *t.last().unwrap() < 100.0);
        let long = sample_times(7200.0);
        assert!(long.len() <= MAX_SAMPLES && long.len() >= MAX_SAMPLES - 1);
    }

    #[test]
    fn boxes_follow_the_samples_smoothly() {
        let s = |t: f64, x: f32| SubjectSample {
            t,
            faces: [x, 0.1, x + 0.2, 0.3],
            people: [x, 0.1, x + 0.3, 0.9],
        };
        let sub = Subject {
            samples: vec![s(10.0, 0.1), s(20.0, 0.5), s(30.0, 0.5)],
        };
        let early = sub.at(0.0).unwrap();
        assert!(
            early.faces[0] <= 0.1 && early.faces[2] >= 0.7,
            "{:?}",
            early.faces
        );
        let late = sub.at(100.0).unwrap().faces;
        assert!((late[0] - 0.5).abs() < 1e-6, "{late:?}");
        let mut prev = sub.at(0.0).unwrap().faces;
        for k in 1..400 {
            let now = sub.at(k as f64 * 0.1).unwrap().faces;
            for i in 0..4 {
                assert!((now[i] - prev[i]).abs() < 0.01, "jump at {k}");
            }
            prev = now;
        }
    }
}
