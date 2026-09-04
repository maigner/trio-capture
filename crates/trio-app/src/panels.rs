use crate::app::App;
use crate::timeline::fmt_time;
use egui::{Color32, RichText};
use std::path::PathBuf;
use trio_core::{Codec, Grade, LayoutId, Orientation};
use trio_media::ffmpeg::HwAccel;

/// The four steps of the sidebar, shown top to bottom in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Open,
    Arrange,
    Colour,
    Export,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Open, Step::Arrange, Step::Colour, Step::Export];

    fn title(self) -> &'static str {
        match self {
            Step::Open => "Open the shoot",
            Step::Arrange => "Arrange the picture",
            Step::Colour => "Match the colours",
            Step::Export => "Export the video",
        }
    }

    fn next(self) -> Option<Step> {
        let i = Step::ALL.iter().position(|s| *s == self)?;
        Step::ALL.get(i + 1).copied()
    }
}

/// Preview decode sizes offered in the View menu.
const PREVIEW_SIZES: [(&str, u32); 4] = [
    ("Fast", 640),
    ("Normal", 960),
    ("Fine", 1280),
    ("Full", 1920),
];

pub fn menu_bar(app: &mut App, root: &mut egui::Ui) {
    let ctx = root.ctx().clone();
    egui::Panel::top("menu").show(root, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open folder…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        app.open_folder(p);
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("New project").clicked() {
                    app.new_project();
                    ui.close();
                }
                if ui.button("Open project…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("trio project", &["json"])
                        .pick_file()
                    {
                        app.open_project(p);
                    }
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    app.save_project(None);
                    ui.close();
                }
                if ui.button("Save as…").clicked() {
                    app.save_project_as();
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                ui.label("Preview quality");
                for (n, e) in PREVIEW_SIZES {
                    if ui
                        .radio(app.preview_max_edge == e, n)
                        .on_hover_text(format!("Decode the cameras at up to {e} pixels"))
                        .clicked()
                        && app.preview_max_edge != e
                    {
                        app.preview_max_edge = e;
                        app.rebuild_streams();
                        ui.close();
                    }
                }
            });
            ui.separator();
            let name = app
                .project_path
                .as_ref()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "untitled".into());
            ui.label(format!("{name}{}", if app.dirty { " *" } else { "" }));
        });
    });
}

pub fn status_bar(app: &mut App, root: &mut egui::Ui) {
    egui::Panel::bottom("status").show(root, |ui| {
        ui.horizontal(|ui| {
            if let Some(job) = &app.export {
                ui.add(
                    egui::ProgressBar::new(job.progress())
                        .desired_width(260.0)
                        .show_percentage(),
                );
                ui.label(job.status());
                if ui.button("Cancel").clicked() {
                    app.export = None;
                    app.status = "Export cancelled".into();
                }
            } else {
                ui.label(&app.status);
                if app.jobs.running > 0 {
                    ui.spinner();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("decode: {}", app.hwaccel.label()));
            });
        });
    });
}

pub fn side_panel(app: &mut App, root: &mut egui::Ui) {
    egui::Panel::left("side")
        .resizable(true)
        .default_size(340.0)
        .show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for step in Step::ALL {
                    step_section(app, ui, step);
                }
            });
        });
}

/// One numbered step: a full-width header with a one-line summary, and the
/// step's controls under it while it is the current step.
fn step_section(app: &mut App, ui: &mut egui::Ui, step: Step) {
    let has_clips = app.project.cameras.iter().any(|c| !c.clips.is_empty());
    let ready = step == Step::Open || has_clips;
    let open = app.step == step;
    let number = Step::ALL.iter().position(|s| *s == step).unwrap_or(0) + 1;
    ui.add_space(4.0);
    ui.add_enabled_ui(ready, |ui| {
        let title = RichText::new(format!("{number}  {}", step.title()))
            .strong()
            .size(16.0);
        let r = ui.add_sized(
            [ui.available_width(), 30.0],
            egui::Button::selectable(open, title),
        );
        let r = if ready {
            r
        } else {
            r.on_disabled_hover_text("Open a shoot first")
        };
        if r.clicked() {
            app.step = step;
        }
        ui.indent(("summary", number), |ui| {
            ui.weak(step_summary(app, step));
        });
    });
    if open {
        ui.add_space(4.0);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                match step {
                    Step::Open => open_step(app, ui),
                    Step::Arrange => arrange_step(app, ui),
                    Step::Colour => colour_step(app, ui),
                    Step::Export => export_step(app, ui),
                }
                if let Some(next) = step.next() {
                    ui.add_space(10.0);
                    ui.add_enabled_ui(has_clips, |ui| {
                        if ui.button(format!("Next: {} ›", next.title())).clicked() {
                            app.step = next;
                        }
                    });
                }
            });
    }
    ui.add_space(4.0);
    ui.separator();
}

/// What the step currently holds, in one short line.
fn step_summary(app: &App, step: Step) -> String {
    match step {
        Step::Open => {
            let cams = app
                .project
                .cameras
                .iter()
                .filter(|c| !c.clips.is_empty())
                .count();
            if cams == 0 {
                return "Nothing opened yet".into();
            }
            let clips: usize = app.project.cameras.iter().map(|c| c.clips.len()).sum();
            let synced = app
                .project
                .cameras
                .iter()
                .flat_map(|c| &c.clips)
                .filter(|c| c.sync_confidence.map(|x| x > 0.0).unwrap_or(false))
                .count();
            let audio = match &app.wav {
                Some(w) => format!("audio {}", fmt_time(w.duration)),
                None if app.project.wav.is_some() => "loading audio…".into(),
                None => "no audio".into(),
            };
            let sync = if app.syncing {
                "syncing…".to_string()
            } else if app.wav.is_none() {
                "not synced".to_string()
            } else {
                format!("{synced} of {clips} clips synced")
            };
            format!("{cams} cameras · {clips} clips · {audio} · {sync}")
        }
        Step::Arrange => {
            let names: Vec<&str> = app
                .project
                .slots
                .iter()
                .map(|s| app.project.cameras[s.camera.min(2)].name.as_str())
                .collect();
            format!("{} · {}", app.project.layout.label(), names.join(", "))
        }
        Step::Colour => {
            if app.grading {
                "matching the cameras…".into()
            } else if app.auto_grades.is_some() {
                "cameras matched automatically".into()
            } else {
                "not matched yet".into()
            }
        }
        Step::Export => {
            let o = &app.project.output;
            let size = match o.width {
                1920 => "Full HD",
                2560 => "2K",
                3840 => "4K",
                _ => "custom size",
            };
            let format = if matches!(
                o.codec,
                Codec::H265Software | Codec::H265Vaapi | Codec::H265VideoToolbox
            ) {
                "Smaller file"
            } else {
                "Standard"
            };
            let file = o
                .path
                .as_ref()
                .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "no output file yet".into());
            format!("{size} · {format} · {file}")
        }
    }
}

fn path_field(
    ui: &mut egui::Ui,
    path: &mut Option<PathBuf>,
    folder: bool,
    filter: Option<(&str, &[&str])>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let mut text = path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let resp = ui
            .add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width() - 70.0));
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !text.is_empty() {
            *path = Some(PathBuf::from(text));
            changed = true;
        }
        if ui.button("Browse").clicked() {
            let mut d = rfd::FileDialog::new();
            if let Some((name, ext)) = filter {
                d = d.add_filter(name, ext);
            }
            let picked = if folder {
                d.pick_folder()
            } else {
                d.pick_file()
            };
            if let Some(p) = picked {
                *path = Some(p);
                changed = true;
            }
        }
    });
    changed
}

fn open_step(app: &mut App, ui: &mut egui::Ui) {
    if !app.ffmpeg_ok {
        ui.colored_label(
            Color32::RED,
            "ffmpeg was not found. Install it and start the app again.",
        );
    }
    ui.label(
        "Pick the folder of the shoot. It holds one folder per camera and the audio \
         recording next to them. The clips are lined up with the audio by themselves.",
    );
    ui.add_space(6.0);
    if ui
        .add_enabled(
            !app.syncing,
            egui::Button::new(RichText::new("Open shoot folder…").strong()),
        )
        .clicked()
    {
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            app.open_folder(p);
        }
    }
    if app.syncing {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Lining up the clips with the audio…");
        });
    }
    ui.add_space(8.0);
    for cam in 0..3 {
        let c = &app.project.cameras[cam];
        if c.clips.is_empty() {
            continue;
        }
        let total: f64 = c.clips.iter().map(|c| c.duration).sum();
        let synced = c
            .clips
            .iter()
            .filter(|c| c.sync_confidence.map(|x| x > 0.0).unwrap_or(false))
            .count();
        let state = if app.syncing {
            String::new()
        } else if app.wav.is_none() {
            " · waiting for audio".into()
        } else if synced == c.clips.len() {
            " · synced".into()
        } else {
            format!(
                " · {} of {} clips not synced",
                c.clips.len() - synced,
                c.clips.len()
            )
        };
        ui.label(format!(
            "{}: {} clips, {}{state}",
            c.name,
            c.clips.len(),
            fmt_time(total)
        ));
    }
    match &app.wav {
        Some(w) => {
            ui.label(format!("Audio: {}", fmt_time(w.duration)));
        }
        None if app.project.wav.is_some() => {
            ui.label("Audio: loading…");
        }
        None if app.project.cameras.iter().any(|c| !c.clips.is_empty()) => {
            ui.colored_label(
                Color32::YELLOW,
                "No audio recording found. Pick it below so the clips can be lined up.",
            );
        }
        None => {}
    }
    if app.clock.player.is_none() {
        ui.colored_label(
            Color32::YELLOW,
            "No sound output was found: playback runs without sound.",
        );
    }
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Pick the folders by hand")
        .default_open(false)
        .show(ui, |ui| {
            ui.weak("Use this when the shoot is not in one folder.");
            for cam in 0..3 {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(format!("Camera {}", cam + 1));
                    ui.add(
                        egui::TextEdit::singleline(&mut app.project.cameras[cam].name)
                            .desired_width(120.0),
                    )
                    .on_hover_text("Name shown in the picture");
                });
                let mut folder = app.project.cameras[cam].folder.clone();
                if path_field(ui, &mut folder, true, None) {
                    if let Some(f) = folder {
                        app.import_camera(cam, f);
                    }
                }
                let c = &app.project.cameras[cam];
                if let Some(first) = c.clips.first() {
                    ui.weak(format!(
                        "{}x{} @ {:.2} fps{}",
                        first.width,
                        first.height,
                        first.fps,
                        if first.hdr { " HDR" } else { "" }
                    ));
                }
            }
            ui.add_space(4.0);
            ui.label("Audio recording");
            let mut wav = app.project.wav.clone();
            if path_field(
                ui,
                &mut wav,
                false,
                Some(("audio", &["wav", "flac", "aif", "aiff", "mp3", "m4a"])),
            ) {
                if let Some(w) = wav {
                    app.import_wav(w);
                }
            }
        });
}

fn arrange_step(app: &mut App, ui: &mut egui::Ui) {
    let mut orientation = app.project.layout.orientation();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut orientation, Orientation::Horizontal, "Horizontal 16:9");
        ui.selectable_value(&mut orientation, Orientation::Vertical, "Vertical 9:16");
    });
    if orientation != app.project.layout.orientation() {
        app.project.layout = LayoutId::ALL
            .iter()
            .copied()
            .find(|l| l.orientation() == orientation)
            .unwrap();
        app.dirty = true;
    }
    ui.add_space(6.0);
    for l in LayoutId::ALL
        .iter()
        .filter(|l| l.orientation() == orientation)
    {
        ui.horizontal(|ui| {
            layout_icon(ui, *l, app.project.layout == *l);
            if ui
                .selectable_label(app.project.layout == *l, l.label())
                .clicked()
            {
                app.project.layout = *l;
                app.dirty = true;
            }
        });
    }
    ui.add_space(10.0);
    ui.heading("Cameras in the picture");
    ui.label("Click a slot in the picture to switch to the next camera, or use the buttons.");
    ui.add_space(4.0);
    layout_picture(app, ui);
    ui.add_space(6.0);
    let names: Vec<String> = app.project.cameras.iter().map(|c| c.name.clone()).collect();
    for s in 0..3 {
        ui.horizontal(|ui| {
            ui.label(format!("Slot {}", s + 1));
            for (i, n) in names.iter().enumerate() {
                if ui
                    .selectable_value(&mut app.project.slots[s].camera, i, n)
                    .changed()
                {
                    app.dirty = true;
                }
            }
        });
    }
    ui.add_space(8.0);
    ui.label("Drag inside a slot in the preview to move the picture, scroll to zoom.");
    ui.horizontal(|ui| {
        if ui
            .button("Rotate cameras")
            .on_hover_text("Every camera moves to the next slot")
            .clicked()
        {
            let c = [
                app.project.slots[0].camera,
                app.project.slots[1].camera,
                app.project.slots[2].camera,
            ];
            app.project.slots[0].camera = c[2];
            app.project.slots[1].camera = c[0];
            app.project.slots[2].camera = c[1];
            app.dirty = true;
        }
        if ui
            .button("Reset framing")
            .on_hover_text("Undo all moving and zooming inside the slots")
            .clicked()
        {
            for slot in app.project.slots.iter_mut() {
                slot.zoom = 1.0;
                slot.pan = [0.0, 0.0];
            }
            app.dirty = true;
        }
    });
}

/// The current layout drawn large, one clickable box per slot with the
/// camera's name in it. A click moves the slot on to the next camera.
fn layout_picture(app: &mut App, ui: &mut egui::Ui) {
    let layout = app.project.layout;
    let w = ui.available_width().min(360.0);
    let h = w * 9.0 / 16.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    // A vertical layout stands upright inside the same box.
    let pic = match layout.orientation() {
        Orientation::Horizontal => rect,
        Orientation::Vertical => {
            let pw = h * 9.0 / 16.0;
            egui::Rect::from_center_size(rect.center(), egui::vec2(pw, h))
        }
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(pic, 2.0, Color32::from_gray(30));
    for (s, r) in trio_core::layout::slot_rects(layout).iter().enumerate() {
        let rr = egui::Rect::from_min_size(
            pic.min + egui::vec2(r.x * pic.width(), r.y * pic.height()),
            egui::vec2(r.w * pic.width(), r.h * pic.height()),
        )
        .shrink(2.0);
        let resp = ui.interact(rr, ui.id().with(("slot_pick", s)), egui::Sense::click());
        if resp.clicked() {
            let slot = &mut app.project.slots[s];
            slot.camera = (slot.camera + 1) % 3;
            app.dirty = true;
        }
        let cam = app.project.slots[s].camera.min(2);
        let fill = if resp.hovered() {
            Color32::from_gray(70)
        } else {
            Color32::from_gray(50)
        };
        painter.rect(
            rr,
            3.0,
            fill,
            egui::Stroke::new(1.0, Color32::from_gray(140)),
            egui::StrokeKind::Inside,
        );
        let name = &app.project.cameras[cam].name;
        let font = if rr.width() < 90.0 || rr.height() < 40.0 {
            egui::FontId::proportional(12.0)
        } else {
            egui::FontId::proportional(16.0)
        };
        painter.text(
            rr.center(),
            egui::Align2::CENTER_CENTER,
            name,
            font,
            Color32::WHITE,
        );
        painter.text(
            rr.left_top() + egui::vec2(5.0, 3.0),
            egui::Align2::LEFT_TOP,
            format!("{}", s + 1),
            egui::FontId::proportional(11.0),
            Color32::from_gray(170),
        );
    }
}

fn layout_icon(ui: &mut egui::Ui, layout: LayoutId, selected: bool) {
    let (w, h) = match layout.orientation() {
        Orientation::Horizontal => (48.0, 27.0),
        Orientation::Vertical => (20.0, 36.0),
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 1.0, Color32::from_gray(30));
    let stroke = if selected {
        Color32::from_rgb(120, 180, 255)
    } else {
        Color32::from_gray(130)
    };
    for r in trio_core::layout::slot_rects(layout) {
        let rr = egui::Rect::from_min_size(
            rect.min + egui::vec2(r.x * w, r.y * h),
            egui::vec2(r.w * w, r.h * h),
        )
        .shrink(0.5);
        p.rect_stroke(
            rr,
            0.0,
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Inside,
        );
    }
}

/// A plain-language slider: a name and what the two ends mean, no number.
struct Look {
    name: &'static str,
    low: &'static str,
    high: &'static str,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
}

impl Look {
    /// Double-click puts the value back to `auto`.
    fn show(&self, ui: &mut egui::Ui, value: &mut f32, auto: f32) -> bool {
        let mut changed = false;
        ui.add_space(4.0);
        ui.label(self.name);
        ui.horizontal(|ui| {
            ui.add_sized(
                [54.0, 18.0],
                egui::Label::new(RichText::new(self.low).weak().small()),
            );
            ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(80.0);
            let resp = ui
                .add(
                    egui::Slider::new(value, self.range.clone())
                        .show_value(false)
                        .logarithmic(self.logarithmic),
                )
                .on_hover_text("Double-click to go back to the automatic value");
            changed |= resp.changed();
            if resp.double_clicked() {
                *value = auto;
                changed = true;
            }
            ui.label(RichText::new(self.high).weak().small());
        });
        changed
    }
}

const fn look(
    name: &'static str,
    low: &'static str,
    high: &'static str,
    range: std::ops::RangeInclusive<f32>,
    logarithmic: bool,
) -> Look {
    Look {
        name,
        low,
        high,
        range,
        logarithmic,
    }
}

fn colour_step(app: &mut App, ui: &mut egui::Ui) {
    ui.label(
        "The cameras are matched to each other automatically. Pick a camera and move the \
         sliders until it looks right; the preview follows.",
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!app.grading, |ui| {
            if ui
                .button("Match cameras automatically")
                .on_hover_text(
                    "Looks at a few frames of every camera and sets brightness, contrast, \
                     colour and warmth so the cameras fit together. Your own changes are \
                     replaced.",
                )
                .clicked()
            {
                app.start_auto_grade();
            }
        });
        if app.grading {
            ui.spinner();
            ui.label(format!(
                "looking at frame {}/{}…",
                app.grade_progress.0, app.grade_progress.1
            ));
        }
    });
    ui.add_space(10.0);
    ui.label("Camera");
    ui.horizontal(|ui| {
        for i in 0..3 {
            let name = app.project.cameras[i].name.clone();
            ui.selectable_value(&mut app.selected_camera, i, name);
        }
    });
    ui.weak("or click a camera in the preview");

    let cam = app.selected_camera.min(2);
    let auto = app.auto_grades.map(|a| a[cam]).unwrap_or_default();
    let mut grade = app.project.cameras[cam].grade;
    let g: &mut Grade = &mut grade;
    let mut changed = false;
    changed |= look("Brightness", "darker", "brighter", -2.0..=2.0, false).show(
        ui,
        &mut g.exposure,
        auto.exposure,
    );
    changed |= look("Contrast", "soft", "punchy", 0.5..=2.0, true).show(
        ui,
        &mut g.contrast,
        auto.contrast,
    );
    changed |= look("Colour", "muted", "vivid", 0.0..=2.0, false).show(
        ui,
        &mut g.saturation,
        auto.saturation,
    );
    changed |= look("Warmth", "cooler", "warmer", -1.0..=1.0, false).show(
        ui,
        &mut g.temperature,
        auto.temperature,
    );
    changed |= look("Tint", "green", "magenta", -1.0..=1.0, false).show(ui, &mut g.tint, auto.tint);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let undo_label = if app.auto_grades.is_some() {
            "Back to automatic"
        } else {
            "Back to neutral"
        };
        if ui
            .add_enabled(*g != auto, egui::Button::new(undo_label))
            .on_hover_text("Throws away your changes to this camera")
            .clicked()
        {
            *g = auto;
            changed = true;
        }
        ui.toggle_value(&mut app.show_original, "Show original")
            .on_hover_text("Preview the cameras as they were recorded, to compare");
    });
    ui.add_space(8.0);
    egui::CollapsingHeader::new("More")
        .default_open(false)
        .show(ui, |ui| {
            ui.label("Shadows, mid-tones and highlights separately.");
            changed |= look("Shadows", "darker", "lighter", -0.25..=0.25, false).show(
                ui,
                &mut g.lift,
                auto.lift,
            );
            changed |= look("Mid-tones", "darker", "lighter", 0.5..=2.0, true).show(
                ui,
                &mut g.gamma,
                auto.gamma,
            );
            changed |= look("Highlights", "darker", "lighter", 0.5..=2.0, true).show(
                ui,
                &mut g.gain,
                auto.gain,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Use this look for all cameras")
                    .on_hover_text("Copies this camera's settings to the other two")
                    .clicked()
                {
                    let copy = *g;
                    for c in app.project.cameras.iter_mut() {
                        c.grade = copy;
                    }
                    changed = true;
                }
                if ui
                    .button("Reset all cameras")
                    .on_hover_text("Removes every colour change from all cameras")
                    .clicked()
                {
                    for c in app.project.cameras.iter_mut() {
                        c.grade = Grade::default();
                    }
                    *g = Grade::default();
                    changed = true;
                }
            });
        });
    app.project.cameras[cam].grade = grade;
    app.dirty |= changed;
}

/// Plain-language quality steps; the number is the CRF/QP handed to ffmpeg.
const QUALITY_STEPS: [(&str, u32, &str); 3] = [
    ("Good", 23, "Smaller file, fine for sharing online"),
    ("Better", 20, "A good balance of size and quality"),
    ("Best", 17, "Largest file, hard to tell from the original"),
];

fn export_step(app: &mut App, ui: &mut egui::Ui) {
    let hw = app.hwaccel;
    let o = &mut app.project.output;
    let mut dirty = false;

    // Size
    ui.label("Size");
    ui.horizontal(|ui| {
        let presets = [
            ("Full HD", 1920u32, 1080u32, "1920 × 1080, plays everywhere"),
            ("2K", 2560, 1440, "2560 × 1440"),
            (
                "4K",
                3840,
                2160,
                "3840 × 2160, largest file, slowest export",
            ),
        ];
        for (n, w, h, hint) in presets {
            let on = o.width == w && o.height == h;
            if ui.selectable_label(on, n).on_hover_text(hint).clicked() && !on {
                o.width = w;
                o.height = h;
                dirty = true;
            }
        }
    });

    // Format: H.264 / H.265 keep the current encoder engine.
    ui.add_space(4.0);
    ui.label("Format");
    ui.horizontal(|ui| {
        let h265 = matches!(
            o.codec,
            Codec::H265Software | Codec::H265Vaapi | Codec::H265VideoToolbox
        );
        if ui
            .selectable_label(!h265, "Standard")
            .on_hover_text("H.264: plays on every device and website")
            .clicked()
            && h265
        {
            o.codec = with_h265(o.codec, false);
            dirty = true;
        }
        if ui
            .selectable_label(h265, "Smaller file")
            .on_hover_text("H.265: about half the size at the same quality, needs a newer player")
            .clicked()
            && !h265
        {
            o.codec = with_h265(o.codec, true);
            dirty = true;
        }
    });

    // Quality
    ui.add_space(4.0);
    ui.label("Quality");
    ui.horizontal(|ui| {
        for (n, q, hint) in QUALITY_STEPS {
            let on = o.quality == q;
            if ui.selectable_label(on, n).on_hover_text(hint).clicked() && !on {
                o.quality = q;
                dirty = true;
            }
        }
    });
    if matches!(o.codec, Codec::H264VideoToolbox | Codec::H265VideoToolbox) {
        ui.weak("The graphics card encoder picks its own quality; this setting has no effect.");
    }

    egui::CollapsingHeader::new("More")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Frame rate");
                egui::ComboBox::from_id_salt("fps")
                    .selected_text(format!("{} fps", o.fps))
                    .show_ui(ui, |ui| {
                        for f in [24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0] {
                            dirty |= ui
                                .selectable_value(&mut o.fps, f, format!("{f} fps"))
                                .changed();
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.label("Encoder");
                let h265 = matches!(
                    o.codec,
                    Codec::H265Software | Codec::H265Vaapi | Codec::H265VideoToolbox
                );
                let gpu = matches!(
                    o.codec,
                    Codec::H264Vaapi
                        | Codec::H265Vaapi
                        | Codec::H264VideoToolbox
                        | Codec::H265VideoToolbox
                );
                if ui
                    .selectable_label(!gpu, "Processor")
                    .on_hover_text("Slower, works on every computer")
                    .clicked()
                    && gpu
                {
                    o.codec = if h265 {
                        Codec::H265Software
                    } else {
                        Codec::H264Software
                    };
                    dirty = true;
                }
                let gpu_codec = gpu_codec(h265, hw);
                ui.add_enabled_ui(gpu_codec.is_some(), |ui| {
                    if ui
                        .selectable_label(gpu, "Graphics card")
                        .on_hover_text(if gpu_codec.is_some() {
                            "Much faster; if the export fails, switch back to Processor"
                        } else {
                            "No graphics card encoder was found on this computer"
                        })
                        .clicked()
                        && !gpu
                    {
                        if let Some(c) = gpu_codec {
                            o.codec = c;
                            dirty = true;
                        }
                    }
                });
            });
        });

    ui.add_space(6.0);
    ui.label("Output file");
    let mut out = o.path.clone();
    ui.horizontal(|ui| {
        let mut text = out
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if ui
            .add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width() - 70.0))
            .changed()
        {
            out = if text.is_empty() {
                None
            } else {
                Some(PathBuf::from(text))
            };
            dirty = true;
        }
        if ui.button("Browse").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("mp4", &["mp4"])
                .set_file_name("band.mp4")
                .save_file()
            {
                out = Some(p);
                dirty = true;
            }
        }
    });
    o.path = out;
    app.dirty |= dirty;

    // Exported part: set only with the I and O keys on the timeline.
    ui.add_space(8.0);
    let dur = app.duration();
    let r = &mut app.project.range;
    if r.end <= r.start {
        r.start = r.start.clamp(0.0, dur);
        r.end = dur;
    }
    let whole = r.start <= 0.0 && r.end >= dur;
    let span = fmt_time(r.end - r.start);
    if whole {
        ui.label(format!("Exports the whole recording ({span})."));
    } else {
        ui.label(format!(
            "Exports from {} to {} ({span}).",
            fmt_time(r.start),
            fmt_time(r.end)
        ));
    }
    ui.weak(
        "To export only a part: play or click on the timeline to where the video should start \
         and press I, then go to where it should end and press O. \
         Home, I, End, O brings back the whole recording.",
    );
    ui.add_space(10.0);

    let can = app.export.is_none() && app.project.output.path.is_some() && dur > 0.0;
    ui.add_enabled_ui(can, |ui| {
        if ui.button(RichText::new("Start export").strong()).clicked() {
            app.start_export();
        }
    });
    if app.project.output.path.is_none() {
        ui.weak("Choose an output file first.");
    }
    if let Some(job) = &app.export {
        ui.add(egui::ProgressBar::new(job.progress()).show_percentage());
        ui.label(job.status());
    }
}

/// Same encoder engine, other format.
fn with_h265(c: Codec, h265: bool) -> Codec {
    match (c, h265) {
        (Codec::H264Software | Codec::H265Software, false) => Codec::H264Software,
        (Codec::H264Software | Codec::H265Software, true) => Codec::H265Software,
        (Codec::H264Vaapi | Codec::H265Vaapi, false) => Codec::H264Vaapi,
        (Codec::H264Vaapi | Codec::H265Vaapi, true) => Codec::H265Vaapi,
        (Codec::H264VideoToolbox | Codec::H265VideoToolbox, false) => Codec::H264VideoToolbox,
        (Codec::H264VideoToolbox | Codec::H265VideoToolbox, true) => Codec::H265VideoToolbox,
    }
}

/// The graphics-card encoder for this platform, if hardware decoding was detected.
fn gpu_codec(h265: bool, hw: HwAccel) -> Option<Codec> {
    match hw {
        HwAccel::None => None,
        HwAccel::Vaapi | HwAccel::VaapiGpuScale => Some(if h265 {
            Codec::H265Vaapi
        } else {
            Codec::H264Vaapi
        }),
        HwAccel::VideoToolbox | HwAccel::VideoToolboxGpuScale => Some(if h265 {
            Codec::H265VideoToolbox
        } else {
            Codec::H264VideoToolbox
        }),
    }
}
