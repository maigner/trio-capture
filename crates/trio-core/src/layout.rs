//! Layout presets: three slot rectangles in normalized output coordinates.
//! Later slots are drawn on top of earlier ones (overlays come last).

use crate::model::LayoutId;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
}

pub fn slot_rects(layout: LayoutId) -> [Rect; 3] {
    match layout {
        LayoutId::HThree => [
            Rect::new(0.0, 0.0, 1.0 / 3.0, 1.0),
            Rect::new(1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
            Rect::new(2.0 / 3.0, 0.0, 1.0 / 3.0, 1.0),
        ],
        LayoutId::HBigLeft => [
            Rect::new(0.0, 0.0, 2.0 / 3.0, 1.0),
            Rect::new(2.0 / 3.0, 0.0, 1.0 / 3.0, 0.5),
            Rect::new(2.0 / 3.0, 0.5, 1.0 / 3.0, 0.5),
        ],
        LayoutId::HBigCenterPip => [
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Rect::new(0.02, 0.68, 0.27, 0.30),
            Rect::new(0.71, 0.68, 0.27, 0.30),
        ],
        LayoutId::VThree => [
            Rect::new(0.0, 0.0, 1.0, 1.0 / 3.0),
            Rect::new(0.0, 1.0 / 3.0, 1.0, 1.0 / 3.0),
            Rect::new(0.0, 2.0 / 3.0, 1.0, 1.0 / 3.0),
        ],
        LayoutId::VBigTop => [
            Rect::new(0.0, 0.0, 1.0, 2.0 / 3.0),
            Rect::new(0.0, 2.0 / 3.0, 0.5, 1.0 / 3.0),
            Rect::new(0.5, 2.0 / 3.0, 0.5, 1.0 / 3.0),
        ],
        LayoutId::VFullPip => [
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Rect::new(0.04, 0.80, 0.36, 0.17),
            Rect::new(0.60, 0.80, 0.36, 0.17),
        ],
    }
}

/// Topmost slot under a normalized point, if any.
pub fn slot_at(layout: LayoutId, px: f32, py: f32) -> Option<usize> {
    slot_rects(layout).iter().rposition(|r| r.contains(px, py))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_win_hit_test() {
        assert_eq!(slot_at(LayoutId::HBigCenterPip, 0.1, 0.8), Some(1));
        assert_eq!(slot_at(LayoutId::HBigCenterPip, 0.5, 0.2), Some(0));
        assert_eq!(slot_at(LayoutId::HThree, 0.9, 0.5), Some(2));
    }
}
