//! Slow automatic pan and zoom on top of the slot framing, so a picture from
//! three static cameras feels alive. The movement is a deterministic
//! function of the master time, so the preview and the export agree, and it
//! is held back by the camera's subject boxes so the faces never leave the
//! picture.

use crate::layout::slot_rects;
use crate::{Project, SubjectSample};
use std::f64::consts::TAU;

/// Extra zoom at full amount (0.12 = up to 12 % closer).
const ZOOM_RANGE: f32 = 0.12;
/// Share of the zoom range that is always applied, so the pan has room.
const ZOOM_FLOOR: f32 = 0.3;
/// Pan drift at full amount, as a share of the visible region.
const PAN_RANGE: f32 = 0.08;
/// Space kept around the face box, as a share of the source frame.
const FACE_MARGIN: f32 = 0.03;
/// Extra space above the faces so no head touches the edge.
const HEADROOM: f32 = 0.05;
/// Space kept around the people box when it fits.
const PEOPLE_MARGIN: f32 = 0.01;

/// Slow waves (period in seconds, weight). The periods share no common
/// multiple below many minutes, so the movement never visibly repeats.
const ZOOM_WAVE: [(f64, f64); 2] = [(41.0, 0.6), (17.0, 0.4)];
const PAN_X_WAVE: [(f64, f64); 2] = [(29.0, 0.6), (11.0, 0.4)];
const PAN_Y_WAVE: [(f64, f64); 2] = [(37.0, 0.6), (13.0, 0.4)];

/// -1..1, smooth in `t`.
fn wave(t: f64, parts: &[(f64, f64)], phase: f64) -> f32 {
    parts
        .iter()
        .enumerate()
        .map(|(i, (period, weight))| weight * (TAU * t / period + phase * (i as f64 + 1.0)).sin())
        .sum::<f64>() as f32
}

/// Share of the source frame (width, height) a slot of `slot_px` pixels
/// shows at zoom 1: cover fit, exactly as the shader does it.
pub fn cover_region(slot_px: (f32, f32), src: (u32, u32)) -> [f32; 2] {
    let slot_aspect = slot_px.0 / slot_px.1.max(1.0);
    let src_aspect = src.0 as f32 / (src.1 as f32).max(1.0);
    if src_aspect > slot_aspect {
        [slot_aspect / src_aspect, 1.0]
    } else {
        [1.0, src_aspect / slot_aspect]
    }
}

fn clamp_center(c: [f32; 2], region: [f32; 2]) -> [f32; 2] {
    [
        c[0].clamp((region[0] * 0.5).min(0.5), (1.0 - region[0] * 0.5).max(0.5)),
        c[1].clamp((region[1] * 0.5).min(0.5), (1.0 - region[1] * 0.5).max(0.5)),
    ]
}

fn box_center(b: &[f32; 4]) -> [f32; 2] {
    [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5]
}

fn grow(b: &[f32; 4], m: [f32; 4]) -> [f32; 4] {
    [
        (b[0] - m[0]).max(0.0),
        (b[1] - m[1]).max(0.0),
        (b[2] + m[2]).min(1.0),
        (b[3] + m[3]).min(1.0),
    ]
}

/// Move `center` the least distance that puts `b` inside the region, on
/// each axis where the box fits the user's own framing (`region0`, which
/// does not change over time, so this never flips mid-movement). Where it
/// does not fit, the framing is the user's business and stays as it is.
/// When the breathing zoom leaves less room than the box needs, the box
/// is centred, which is where the least-distance move ends up anyway.
fn keep_inside(center: [f32; 2], region: [f32; 2], region0: [f32; 2], b: [f32; 4]) -> [f32; 2] {
    let mut c = center;
    for a in 0..2 {
        let (lo, hi) = (b[a], b[a + 2]);
        if hi - lo > region0[a] {
            continue;
        }
        let (min, max) = (hi - region[a] * 0.5, lo + region[a] * 0.5);
        c[a] = if min <= max {
            c[a].clamp(min, max)
        } else {
            (lo + hi) * 0.5
        };
    }
    c
}

/// The face box with its margins, the part that must stay in view.
fn face_zone(s: &SubjectSample) -> [f32; 4] {
    grow(
        &s.faces,
        [
            FACE_MARGIN,
            FACE_MARGIN + HEADROOM,
            FACE_MARGIN,
            FACE_MARGIN,
        ],
    )
}

/// Effective zoom and pan of slot `index` at master time `t`, given the
/// size of the camera frame in that slot. The slot's own zoom and pan are
/// the starting point; with the movement off they come back unchanged.
pub fn framing(project: &Project, index: usize, src: (u32, u32), t: f64) -> (f32, [f32; 2]) {
    let slot = project.slots[index];
    let amount = project.motion.amount();
    if amount <= 0.0 {
        return (slot.zoom, slot.pan);
    }
    let r = slot_rects(project.layout)[index];
    let (out_w, out_h) = project.output_size();
    let base = cover_region((r.w * out_w as f32, r.h * out_h as f32), src);
    let zoom0 = slot.zoom.max(0.01);
    let region0 = [base[0] / zoom0, base[1] / zoom0];
    let center0 = clamp_center([0.5 + slot.pan[0], 0.5 + slot.pan[1]], region0);
    let subject = project
        .cameras
        .get(slot.camera)
        .and_then(|c| c.subject.as_ref())
        .and_then(|s| s.at(t));

    // Breathe in and out, never further than the faces allow.
    let phase = index as f64 * 2.4;
    let u = 0.5 + 0.5 * wave(t, &ZOOM_WAVE, phase);
    let mut zoom = zoom0 * (1.0 + ZOOM_RANGE * amount * (ZOOM_FLOOR + (1.0 - ZOOM_FLOOR) * u));
    if let Some(s) = &subject {
        let z = face_zone(s);
        let fits = (base[0] / (z[2] - z[0]).max(1e-3)).min(base[1] / (z[3] - z[1]).max(1e-3));
        zoom = zoom.min(fits.max(zoom0));
    }
    let region = [base[0] / zoom, base[1] / zoom];

    // Zoom toward the faces so they keep their place in the picture, as
    // long as the user framed them in; otherwise toward the user's centre.
    let mut anchor = center0;
    if let Some(s) = &subject {
        let fc = box_center(&s.faces);
        for a in 0..2 {
            if (fc[a] - center0[a]).abs() <= region0[a] * 0.5 {
                anchor[a] = fc[a];
            }
        }
    }
    let mut center = [0.0; 2];
    for a in 0..2 {
        let k = region[a] / region0[a].max(1e-6);
        center[a] = anchor[a] + (center0[a] - anchor[a]) * k;
    }
    // Drift sideways and up and down.
    center[0] += PAN_RANGE * amount * region[0] * wave(t, &PAN_X_WAVE, phase + 0.7);
    center[1] += PAN_RANGE * amount * region[1] * wave(t, &PAN_Y_WAVE, phase + 1.9);
    if let Some(s) = &subject {
        center = keep_inside(center, region, region0, grow(&s.people, [PEOPLE_MARGIN; 4]));
        center = keep_inside(center, region, region0, face_zone(s));
    }
    let center = clamp_center(center, region);
    (zoom, [center[0] - 0.5, center[1] - 0.5])
}

/// The part of the source frame (x0, y0, x1, y1) slot `index` shows at
/// master time `t`, movement included.
pub fn visible_box(project: &Project, index: usize, src: (u32, u32), t: f64) -> [f32; 4] {
    let (zoom, pan) = framing(project, index, src, t);
    let r = slot_rects(project.layout)[index];
    let (out_w, out_h) = project.output_size();
    let base = cover_region((r.w * out_w as f32, r.h * out_h as f32), src);
    let region = [base[0] / zoom.max(0.01), base[1] / zoom.max(0.01)];
    let c = clamp_center([0.5 + pan[0], 0.5 + pan[1]], region);
    [
        c[0] - region[0] * 0.5,
        c[1] - region[1] * 0.5,
        c[0] + region[0] * 0.5,
        c[1] + region[1] * 0.5,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Motion, Project, Subject};

    fn subject(faces: [f32; 4], people: [f32; 4]) -> Subject {
        Subject {
            samples: vec![SubjectSample {
                t: 0.0,
                faces,
                people,
            }],
        }
    }

    fn project(subject: Option<Subject>) -> Project {
        let mut p = Project {
            motion: Motion::Lively,
            ..Default::default()
        };
        for c in p.cameras.iter_mut() {
            c.subject = subject.clone();
        }
        p
    }

    fn contains(outer: [f32; 4], inner: [f32; 4]) -> bool {
        outer[0] <= inner[0] + 1e-4
            && outer[1] <= inner[1] + 1e-4
            && outer[2] >= inner[2] - 1e-4
            && outer[3] >= inner[3] - 1e-4
    }

    #[test]
    fn off_means_untouched() {
        let mut p = project(None);
        p.motion = Motion::Off;
        p.slots[1].zoom = 1.5;
        p.slots[1].pan = [0.1, -0.2];
        assert_eq!(framing(&p, 1, (1920, 1080), 12.3), (1.5, [0.1, -0.2]));
    }

    #[test]
    fn moves_but_stays_in_frame() {
        let p = project(None);
        let mut zooms = Vec::new();
        for k in 0..600 {
            let t = k as f64 * 0.5;
            let b = visible_box(&p, 0, (1920, 1080), t);
            assert!(b[0] >= -1e-4 && b[1] >= -1e-4 && b[2] <= 1.0 + 1e-4 && b[3] <= 1.0 + 1e-4);
            zooms.push(framing(&p, 0, (1920, 1080), t).0);
        }
        let (lo, hi) = zooms
            .iter()
            .fold((f32::MAX, f32::MIN), |(l, h), &z| (l.min(z), h.max(z)));
        assert!(lo > 1.0 && hi < 1.13 && hi - lo > 0.03, "{lo}..{hi}");
    }

    #[test]
    fn faces_never_leave_the_picture() {
        let faces = [0.55, 0.15, 0.85, 0.40];
        let p = project(Some(subject(faces, [0.45, 0.15, 0.95, 0.95])));
        for slot in 0..3 {
            for k in 0..600 {
                let b = visible_box(&p, slot, (1920, 1080), k as f64 * 0.5);
                assert!(contains(b, faces), "slot {slot} t {k}: {b:?}");
            }
        }
    }

    #[test]
    fn a_wide_face_box_stops_the_zoom() {
        let p = project(Some(subject([0.02, 0.1, 0.98, 0.5], [0.0, 0.1, 1.0, 1.0])));
        for k in 0..200 {
            let (zoom, _) = framing(&p, 0, (1920, 1080), k as f64 * 0.5);
            assert!(zoom < 1.001, "{zoom}");
            let b = visible_box(&p, 0, (1920, 1080), k as f64 * 0.5);
            assert!(b[0] >= -1e-4 && b[2] <= 1.0 + 1e-4, "{b:?}");
        }
    }

    #[test]
    fn a_box_that_does_not_fit_leaves_the_framing_alone() {
        // The user zoomed far into the lower right; the faces are elsewhere.
        let mut p = project(Some(subject([0.1, 0.1, 0.9, 0.4], [0.0, 0.1, 1.0, 1.0])));
        p.slots[0].zoom = 3.0;
        p.slots[0].pan = [0.3, 0.3];
        for k in 0..200 {
            let b = visible_box(&p, 0, (1920, 1080), k as f64 * 0.5);
            let c = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
            assert!(
                (c[0] - 0.8).abs() < 0.06 && (c[1] - 0.8).abs() < 0.06,
                "{c:?}"
            );
        }
    }

    #[test]
    fn movement_is_smooth() {
        let p = project(Some(subject([0.3, 0.2, 0.6, 0.4], [0.2, 0.2, 0.8, 0.9])));
        let mut prev = framing(&p, 2, (3840, 2160), 0.0);
        for k in 1..3000 {
            let now = framing(&p, 2, (3840, 2160), k as f64 / 30.0);
            assert!((now.0 - prev.0).abs() < 0.002, "zoom jump at {k}");
            assert!((now.1[0] - prev.1[0]).abs() < 0.002, "pan jump at {k}");
            prev = now;
        }
    }
}
