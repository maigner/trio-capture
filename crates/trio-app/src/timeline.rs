//! Timeline: ruler, WAV waveform, one clip track per camera.

use crate::jobs::WavData;
use egui::{Color32, Pos2, Rect, Sense, Stroke};
use trio_core::sync::SYNC_RATE;
use trio_core::Project;

pub struct Timeline {
    pub start: f64,
    pub end: f64,
    initialized: bool,
    wave_cache: Option<(f64, f64, usize, Vec<(f32, f32)>)>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: 60.0,
            initialized: false,
            wave_cache: None,
        }
    }
}

const RULER_H: f32 = 18.0;
const WAVE_H: f32 = 46.0;
const TRACK_H: f32 = 26.0;

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor();
    let s = t - m * 60.0;
    format!("{m:02.0}:{s:05.2}")
}

impl Timeline {
    pub fn reset_view(&mut self) {
        self.initialized = false;
        self.wave_cache = None;
    }

    /// Returns a requested seek time. Sets `changed` when a clip was moved.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        project: &mut Project,
        wav: Option<&WavData>,
        playhead: f64,
        duration: f64,
        changed: &mut bool,
    ) -> Option<f64> {
        let mut seek = None;
        // Transport row.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(fmt_time(playhead)).monospace().size(18.0));
            ui.label(egui::RichText::new(format!("/ {}", fmt_time(duration))).monospace());
            ui.separator();
            ui.label("Space play/pause · Left/Right frame · Shift+Left/Right second · Home/End · I/O range · Ctrl+scroll zoom · Shift+scroll pan · drag clips to nudge");
        });

        if !self.initialized || self.end <= self.start {
            self.start = 0.0;
            self.end = duration.max(10.0) * 1.02;
            self.initialized = true;
        }

        let height = RULER_H + WAVE_H + TRACK_H * 3.0 + 6.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, Color32::from_gray(22));

        let span = (self.end - self.start).max(0.01);
        let t_of = |x: f32| self.start + ((x - rect.min.x) / rect.width()) as f64 * span;

        // Zoom / pan with the wheel.
        if ui.rect_contains_pointer(rect) {
            let (scroll, mods, pos) =
                ui.input(|i| (i.smooth_scroll_delta, i.modifiers, i.pointer.hover_pos()));
            if mods.command || mods.ctrl {
                if scroll.y != 0.0 {
                    let anchor = pos.map(|p| t_of(p.x)).unwrap_or(playhead);
                    let factor = (1.0 - scroll.y as f64 * 0.003).clamp(0.5, 2.0);
                    let new_span = (span * factor).clamp(0.5, duration.max(10.0) * 4.0);
                    let frac = (anchor - self.start) / span;
                    self.start = anchor - frac * new_span;
                    self.end = self.start + new_span;
                }
            } else {
                let dx = if scroll.x != 0.0 {
                    scroll.x
                } else if mods.shift {
                    scroll.y
                } else {
                    0.0
                };
                if dx != 0.0 {
                    let dt = -(dx as f64) / rect.width() as f64 * span;
                    self.start += dt;
                    self.end += dt;
                }
            }
            let min_start = -span * 0.1;
            if self.start < min_start {
                self.end += min_start - self.start;
                self.start = min_start;
            }
        }
        let span = (self.end - self.start).max(0.01);
        let x_of = |t: f64| rect.min.x + ((t - self.start) / span) as f32 * rect.width();
        let t_of = |x: f32| self.start + ((x - rect.min.x) / rect.width()) as f64 * span;

        // Ruler.
        let ruler = Rect::from_min_size(rect.min, egui::vec2(rect.width(), RULER_H));
        let step = [
            0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0,
        ]
        .into_iter()
        .find(|s| (s / span) as f32 * rect.width() >= 70.0)
        .unwrap_or(600.0);
        let mut t = (self.start / step).floor() * step;
        while t <= self.end {
            let x = x_of(t);
            if x >= rect.min.x && x <= rect.max.x {
                painter.line_segment(
                    [Pos2::new(x, ruler.min.y), Pos2::new(x, rect.max.y)],
                    Stroke::new(1.0, Color32::from_gray(45)),
                );
                painter.text(
                    Pos2::new(x + 3.0, ruler.min.y + 2.0),
                    egui::Align2::LEFT_TOP,
                    fmt_time(t),
                    egui::FontId::monospace(10.0),
                    Color32::from_gray(160),
                );
            }
            t += step;
        }

        // Waveform.
        let wave = Rect::from_min_size(
            Pos2::new(rect.min.x, ruler.max.y),
            egui::vec2(rect.width(), WAVE_H),
        );
        painter.rect_filled(wave, 0.0, Color32::from_gray(28));
        if let Some(w) = wav {
            let cols = rect.width() as usize;
            let cached = self
                .wave_cache
                .as_ref()
                .filter(|(s, e, c, _)| *s == self.start && *e == self.end && *c == cols);
            if cached.is_none() {
                let peaks: Vec<(f32, f32)> = (0..cols)
                    .map(|c| {
                        let t0 = t_of(rect.min.x + c as f32);
                        let t1 = t_of(rect.min.x + c as f32 + 1.0);
                        let s0 = (t0 * SYNC_RATE as f64).max(0.0) as usize;
                        let s1 = ((t1 * SYNC_RATE as f64).max(0.0) as usize).min(w.mono8k.len());
                        if s0 >= s1 {
                            return (0.0, 0.0);
                        }
                        w.mono8k[s0..s1]
                            .iter()
                            .fold((0.0f32, 0.0f32), |(lo, hi), &x| (lo.min(x), hi.max(x)))
                    })
                    .collect();
                self.wave_cache = Some((self.start, self.end, cols, peaks));
            }
            let peaks = &self.wave_cache.as_ref().unwrap().3;
            let mid = wave.center().y;
            let amp = WAVE_H * 0.48;
            for (c, (lo, hi)) in peaks.iter().enumerate() {
                let x = rect.min.x + c as f32;
                painter.line_segment(
                    [Pos2::new(x, mid - hi * amp), Pos2::new(x, mid - lo * amp)],
                    Stroke::new(1.0, Color32::from_rgb(90, 160, 210)),
                );
            }
        } else {
            painter.text(
                wave.left_center() + egui::vec2(8.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "no WAV loaded",
                egui::FontId::proportional(11.0),
                Color32::from_gray(110),
            );
        }

        // Click / drag on ruler or waveform seeks.
        let seek_area = Rect::from_min_max(rect.min, wave.max);
        let seek_resp = ui.interact(seek_area, ui.id().with("seek"), Sense::click_and_drag());
        if seek_resp.clicked() || seek_resp.dragged() {
            if let Some(p) = seek_resp.interact_pointer_pos() {
                seek = Some(t_of(p.x).clamp(0.0, duration.max(0.0)));
            }
        }

        // Camera tracks.
        let colors = [
            Color32::from_rgb(200, 120, 90),
            Color32::from_rgb(110, 190, 120),
            Color32::from_rgb(120, 140, 220),
        ];
        let frame = 1.0 / project.output.fps;
        for cam in 0..3 {
            let y0 = wave.max.y + cam as f32 * TRACK_H + 2.0;
            let track = Rect::from_min_size(
                Pos2::new(rect.min.x, y0),
                egui::vec2(rect.width(), TRACK_H - 2.0),
            );
            painter.rect_filled(track, 0.0, Color32::from_gray(26));
            painter.text(
                track.left_center() + egui::vec2(4.0, 0.0),
                egui::Align2::LEFT_CENTER,
                &project.cameras[cam].name,
                egui::FontId::proportional(10.0),
                Color32::from_gray(120),
            );
            let shift_all = ui.input(|i| i.modifiers.shift);
            let mut shift_delta: Option<f64> = None;
            let n = project.cameras[cam].clips.len();
            for idx in 0..n {
                let clip = project.cameras[cam].clips[idx].clone();
                let x0 = x_of(clip.offset).max(rect.min.x);
                let x1 = x_of(clip.end()).min(rect.max.x);
                if x1 <= rect.min.x || x0 >= rect.max.x {
                    continue;
                }
                let block = Rect::from_min_max(
                    Pos2::new(x0, track.min.y + 2.0),
                    Pos2::new(x1, track.max.y - 2.0),
                );
                let id = ui.id().with(("clip", cam, idx));
                let r = ui.interact(block, id, Sense::click_and_drag());
                let mut fill = colors[cam].gamma_multiply(if r.hovered() { 0.9 } else { 0.65 });
                if let Some(c) = clip.sync_confidence {
                    if c < 0.3 {
                        fill = Color32::from_rgb(180, 70, 70);
                    }
                }
                painter.rect_filled(block, 3.0, fill);
                painter.rect_stroke(
                    block,
                    3.0,
                    Stroke::new(1.0, Color32::from_black_alpha(120)),
                    egui::StrokeKind::Inside,
                );
                let label = format!(
                    "{}  {}",
                    clip.file_name(),
                    clip.sync_confidence
                        .map(|c| format!("{:.0}%", c * 100.0))
                        .unwrap_or_default()
                );
                painter.with_clip_rect(block).text(
                    block.left_center() + egui::vec2(4.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
                if r.dragged() {
                    let dt = r.drag_delta().x as f64 / rect.width() as f64 * span;
                    if dt != 0.0 {
                        if shift_all {
                            shift_delta = Some(dt);
                        } else {
                            project.cameras[cam].clips[idx].offset += dt;
                            *changed = true;
                        }
                    }
                }
                if r.drag_stopped() {
                    // Snap to the frame grid when the drag ends.
                    let c = &mut project.cameras[cam].clips[idx];
                    c.offset = (c.offset / frame).round() * frame;
                    *changed = true;
                }
                r.on_hover_text(format!("{}\noffset {:.3}s, {:.1}s long\ndrag to nudge, Shift+drag moves all clips of this camera", clip.file_name(), clip.offset, clip.duration));
            }
            if let Some(dt) = shift_delta {
                for c in project.cameras[cam].clips.iter_mut() {
                    c.offset += dt;
                }
                *changed = true;
            }
        }

        // Range shading and playhead.
        let (rs, re) = (project.range.start, project.range.end);
        if re > rs {
            let shade = Color32::from_black_alpha(90);
            painter.rect_filled(
                Rect::from_min_max(
                    rect.min,
                    Pos2::new(x_of(rs).clamp(rect.min.x, rect.max.x), rect.max.y),
                ),
                0.0,
                shade,
            );
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x_of(re).clamp(rect.min.x, rect.max.x), rect.min.y),
                    rect.max,
                ),
                0.0,
                shade,
            );
        }
        let px = x_of(playhead);
        if px >= rect.min.x && px <= rect.max.x {
            painter.line_segment(
                [Pos2::new(px, rect.min.y), Pos2::new(px, rect.max.y)],
                Stroke::new(1.5, Color32::from_rgb(255, 80, 80)),
            );
        }
        seek
    }
}
