//! Composed preview with direct manipulation of the slots.

use crate::app::App;
use egui::{Color32, Rect, Sense, Stroke};
use trio_core::layout::{slot_at, slot_rects};
use trio_core::Orientation;

/// Fraction of the source visible in a slot at zoom 1 (mirrors the shader).
fn visible_region(slot_px: egui::Vec2, src: (u32, u32), zoom: f32) -> egui::Vec2 {
    let slot_aspect = slot_px.x / slot_px.y.max(1.0);
    let src_aspect = src.0 as f32 / src.1.max(1) as f32;
    let mut region = egui::vec2(1.0, 1.0);
    if src_aspect > slot_aspect {
        region.x = slot_aspect / src_aspect;
    } else {
        region.y = src_aspect / slot_aspect;
    }
    region / zoom.max(0.01)
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let avail = ui.available_rect_before_wrap();
    let (pw, ph) = (app.preview.width as f32, app.preview.height as f32);
    let scale = (avail.width() / pw).min(avail.height() / ph);
    let size = egui::vec2(pw * scale, ph * scale);
    let rect = Rect::from_center_size(avail.center(), size);

    let resp = ui.put(
        rect,
        egui::Image::new(egui::load::SizedTexture::new(app.preview_tex, size))
            .sense(Sense::click_and_drag()),
    );
    let painter = ui.painter_at(rect);
    let layout = app.project.layout;
    let rects = slot_rects(layout);

    let to_norm = |p: egui::Pos2| {
        (
            (p.x - rect.min.x) / rect.width(),
            (p.y - rect.min.y) / rect.height(),
        )
    };
    let hovered = resp
        .hover_pos()
        .map(to_norm)
        .and_then(|(x, y)| slot_at(layout, x, y));
    let drag_slot_id = ui.id().with("drag_slot");
    if resp.drag_started() {
        let s = resp
            .interact_pointer_pos()
            .map(to_norm)
            .and_then(|(x, y)| slot_at(layout, x, y));
        ui.data_mut(|d| d.insert_temp(drag_slot_id, s));
    }
    let drag_slot: Option<usize> = if resp.dragged() {
        ui.data(|d| d.get_temp(drag_slot_id)).flatten()
    } else {
        None
    };

    // Pan by dragging.
    if let Some(s) = drag_slot {
        let delta = resp.drag_delta();
        if delta != egui::Vec2::ZERO {
            let r = rects[s];
            let slot_px = egui::vec2(r.w * pw, r.h * ph);
            let slot_screen = egui::vec2(r.w * rect.width(), r.h * rect.height());
            let cam = app.project.slots[s].camera.min(2);
            if let Some(src) = app.preview.src_sizes[cam] {
                let region = visible_region(slot_px, src, app.project.slots[s].zoom);
                let slot = &mut app.project.slots[s];
                slot.pan[0] = (slot.pan[0] - delta.x / slot_screen.x * region.x).clamp(-0.5, 0.5);
                slot.pan[1] = (slot.pan[1] - delta.y / slot_screen.y * region.y).clamp(-0.5, 0.5);
                app.dirty = true;
            }
        }
    }
    // Zoom by scrolling.
    if let Some(s) = hovered {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let slot = &mut app.project.slots[s];
            slot.zoom = (slot.zoom * (1.0 + scroll * 0.002)).clamp(1.0, 4.0);
            app.dirty = true;
        }
    }
    if resp.clicked() {
        if let Some(s) = hovered {
            app.selected_camera = app.project.slots[s].camera.min(2);
        }
    }

    // Overlays: slot outlines and labels.
    for (i, r) in rects.iter().enumerate() {
        let rr = Rect::from_min_size(
            rect.min + egui::vec2(r.x * rect.width(), r.y * rect.height()),
            egui::vec2(r.w * rect.width(), r.h * rect.height()),
        );
        let cam = app.project.slots[i].camera.min(2);
        let selected = cam == app.selected_camera && app.tab == crate::panels::Tab::Grade;
        let color = if hovered == Some(i) || drag_slot == Some(i) {
            Color32::from_rgb(120, 180, 255)
        } else if selected {
            Color32::from_rgb(255, 200, 80)
        } else {
            Color32::from_white_alpha(40)
        };
        painter.rect_stroke(
            rr.shrink(0.5),
            0.0,
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        let label = format!(
            "{}{}",
            app.project.cameras[cam].name,
            if app.project.slots[i].zoom > 1.001 {
                format!("  {:.2}x", app.project.slots[i].zoom)
            } else {
                String::new()
            }
        );
        painter.text(
            rr.min + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::proportional(12.0),
            Color32::from_white_alpha(180),
        );
    }
    let hint = match app.project.layout.orientation() {
        Orientation::Horizontal => "16:9",
        Orientation::Vertical => "9:16",
    };
    painter.text(
        rect.right_bottom() - egui::vec2(6.0, 4.0),
        egui::Align2::RIGHT_BOTTOM,
        format!(
            "{} · {}x{} preview",
            hint, app.preview.width, app.preview.height
        ),
        egui::FontId::proportional(11.0),
        Color32::from_white_alpha(120),
    );
}
