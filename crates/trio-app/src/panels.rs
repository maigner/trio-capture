use crate::app::App;
use egui::{Color32, RichText};
use std::path::PathBuf;
use trio_core::{Codec, Grade, LayoutId, Orientation, Slot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Import,
    Layout,
    Grade,
    Export,
}

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
            ui.horizontal_wrapped(|ui| {
                for (tab, label) in [
                    (Tab::Import, "Import"),
                    (Tab::Layout, "Layout"),
                    (Tab::Grade, "Grade"),
                    (Tab::Export, "Export"),
                ] {
                    ui.selectable_value(&mut app.tab, tab, label);
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match app.tab {
                Tab::Import => import_tab(app, ui),
                Tab::Layout => layout_tab(app, ui),
                Tab::Grade => grade_tab(app, ui),
                Tab::Export => export_tab(app, ui),
            });
        });
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

fn import_tab(app: &mut App, ui: &mut egui::Ui) {
    if !app.ffmpeg_ok {
        ui.colored_label(
            Color32::RED,
            "ffmpeg was not found on PATH. Install it and restart.",
        );
    }
    ui.heading("Shoot folder");
    ui.label(
        "One folder with a subfolder per camera and the master audio file next to them. \
         Cameras are found, the audio is loaded and every clip is synced to it.",
    );
    if ui
        .add_enabled(!app.syncing, egui::Button::new("Open folder…"))
        .clicked()
    {
        if let Some(p) = rfd::FileDialog::new().pick_folder() {
            app.open_folder(p);
        }
    }
    ui.add_space(10.0);
    ui.heading("Cameras");
    ui.label(
        "Or pick each folder by hand. Every video file inside becomes a clip; \
         clips are synced to the audio as soon as both are loaded.",
    );
    for cam in 0..3 {
        ui.add_space(6.0);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{}", cam + 1));
                ui.text_edit_singleline(&mut app.project.cameras[cam].name);
            });
            let mut folder = app.project.cameras[cam].folder.clone();
            if path_field(ui, &mut folder, true, None) {
                if let Some(f) = folder {
                    app.import_camera(cam, f);
                }
            }
            let c = &app.project.cameras[cam];
            let total: f64 = c.clips.iter().map(|c| c.duration).sum();
            let summary = format!("{} clips, {:.0}s", c.clips.len(), total);
            let format = c.clips.first().map(|first| {
                format!(
                    "{}x{} @ {:.2} fps{}",
                    first.width,
                    first.height,
                    first.fps,
                    if first.hdr { " HDR" } else { "" }
                )
            });
            let folder = c.folder.clone();
            ui.horizontal(|ui| {
                ui.label(summary);
                if let Some(format) = format {
                    ui.label(format);
                }
                if let Some(folder) = folder {
                    if ui.small_button("Rescan").clicked() {
                        app.import_camera(cam, folder);
                    }
                }
            });
        });
    }
    ui.add_space(10.0);
    ui.heading("Audio");
    ui.label("Master WAV. Its timeline is the master clock.");
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
    if let Some(w) = &app.wav {
        ui.label(format!("{:.1}s loaded", w.duration));
    }
    ui.add_space(10.0);
    ui.heading("Preview");
    ui.horizontal(|ui| {
        ui.label("Decode size");
        let mut edge = app.preview_max_edge;
        egui::ComboBox::from_id_salt("preview_edge")
            .selected_text(format!("{edge}px"))
            .show_ui(ui, |ui| {
                for e in [640u32, 960, 1280, 1920] {
                    ui.selectable_value(&mut edge, e, format!("{e}px"));
                }
            });
        if edge != app.preview_max_edge {
            app.preview_max_edge = edge;
            app.rebuild_streams();
        }
    });
    if app.clock.player.is_none() {
        ui.colored_label(
            Color32::YELLOW,
            "No audio output device: playback runs on a silent wall clock.",
        );
    }
}

fn layout_tab(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Layout");
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
    ui.heading("Slots");
    ui.label("Drag inside a slot in the preview to pan, scroll to zoom.");
    for s in 0..3 {
        ui.add_space(4.0);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Slot {}", s + 1));
                let slot: &mut Slot = &mut app.project.slots[s];
                let names: Vec<String> =
                    app.project.cameras.iter().map(|c| c.name.clone()).collect();
                egui::ComboBox::from_id_salt(("slot_cam", s))
                    .selected_text(&names[slot.camera.min(2)])
                    .show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            if ui.selectable_value(&mut slot.camera, i, n).changed() {
                                app.dirty = true;
                            }
                        }
                    });
                if ui.small_button("Reset").clicked() {
                    slot.zoom = 1.0;
                    slot.pan = [0.0, 0.0];
                    app.dirty = true;
                }
            });
            let slot = &mut app.project.slots[s];
            app.dirty |= ui
                .add(egui::Slider::new(&mut slot.zoom, 1.0..=4.0).text("zoom"))
                .changed();
            app.dirty |= ui
                .add(egui::Slider::new(&mut slot.pan[0], -0.5..=0.5).text("pan x"))
                .changed();
            app.dirty |= ui
                .add(egui::Slider::new(&mut slot.pan[1], -0.5..=0.5).text("pan y"))
                .changed();
        });
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Rotate cameras").clicked() {
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
    });
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

fn grade_tab(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Grade");
    ui.label("Grades belong to a camera and show in every slot it occupies. Click a slot in the preview to pick its camera.");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!app.grading, |ui| {
            if ui
                .button("Auto grade all cameras")
                .on_hover_text(
                    "Measures a few frames of every camera and sets exposure, contrast, \
                     saturation and colour balance so the cameras match. Runs by itself \
                     after sync while the grades are untouched.",
                )
                .clicked()
            {
                app.start_auto_grade();
            }
        });
        if app.grading {
            ui.spinner();
            ui.label(format!(
                "Analysing {}/{} frames…",
                app.grade_progress.0, app.grade_progress.1
            ));
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for i in 0..3 {
            let name = app.project.cameras[i].name.clone();
            ui.selectable_value(&mut app.selected_camera, i, name);
        }
    });
    ui.add_space(6.0);
    let cam = app.selected_camera.min(2);
    let mut grade = app.project.cameras[cam].grade;
    let g: &mut Grade = &mut grade;
    let mut changed = false;
    changed |= ui
        .add(egui::Slider::new(&mut g.exposure, -3.0..=3.0).text("exposure (stops)"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut g.contrast, 0.5..=2.0).text("contrast"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut g.saturation, 0.0..=2.0).text("saturation"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut g.temperature, -1.0..=1.0).text("temperature"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut g.tint, -1.0..=1.0).text("tint"))
        .changed();
    ui.separator();
    changed |= ui
        .add(egui::Slider::new(&mut g.lift, -0.25..=0.25).text("lift (shadows)"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut g.gamma, 0.5..=2.0).text("gamma (mids)"))
        .changed();
    changed |= ui
        .add(egui::Slider::new(&mut g.gain, 0.5..=2.0).text("gain (highlights)"))
        .changed();
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Reset").clicked() {
            *g = Grade::default();
            changed = true;
        }
        if ui.button("Copy to other cameras").clicked() {
            let copy = *g;
            for c in app.project.cameras.iter_mut() {
                c.grade = copy;
            }
            changed = true;
        }
    });
    app.project.cameras[cam].grade = grade;
    app.dirty |= changed;
}

fn export_tab(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Export");
    let o = &mut app.project.output;
    ui.horizontal(|ui| {
        ui.label("Resolution");
        let presets = [
            ("1080p", 1920u32, 1080u32),
            ("1440p", 2560, 1440),
            ("4K", 3840, 2160),
        ];
        let cur = presets
            .iter()
            .find(|p| p.1 == o.width && p.2 == o.height)
            .map(|p| p.0)
            .unwrap_or("custom");
        egui::ComboBox::from_id_salt("res")
            .selected_text(cur)
            .show_ui(ui, |ui| {
                for (n, w, h) in presets {
                    if ui
                        .selectable_label(o.width == w && o.height == h, n)
                        .clicked()
                    {
                        o.width = w;
                        o.height = h;
                        app.dirty = true;
                    }
                }
            });
        ui.label("fps");
        egui::ComboBox::from_id_salt("fps")
            .selected_text(format!("{}", o.fps))
            .show_ui(ui, |ui| {
                for f in [24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0] {
                    if ui.selectable_value(&mut o.fps, f, format!("{f}")).changed() {
                        app.dirty = true;
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Codec");
        egui::ComboBox::from_id_salt("codec")
            .selected_text(o.codec.label())
            .show_ui(ui, |ui| {
                for c in Codec::ALL {
                    if ui.selectable_value(&mut o.codec, c, c.label()).changed() {
                        app.dirty = true;
                    }
                }
            });
    });
    let q_label = match o.codec {
        Codec::H264Software | Codec::H265Software => {
            "CRF (lower = better, 18 is visually lossless)"
        }
        Codec::H264Vaapi | Codec::H265Vaapi => "QP (lower = better)",
        _ => "quality (unused for VideoToolbox, bitrate is automatic)",
    };
    app.dirty |= ui
        .add(egui::Slider::new(&mut o.quality, 10..=35).text(q_label))
        .changed();
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
            app.dirty = true;
        }
        if ui.button("Browse").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("mp4", &["mp4"])
                .set_file_name("band.mp4")
                .save_file()
            {
                out = Some(p);
                app.dirty = true;
            }
        }
    });
    app.project.output.path = out;

    ui.add_space(8.0);
    ui.heading("Range");
    let t = app.clock.time();
    let dur = app.duration();
    let r = &mut app.project.range;
    if r.end <= r.start {
        r.end = dur;
    }
    ui.horizontal(|ui| {
        ui.label("in");
        app.dirty |= ui
            .add(
                egui::DragValue::new(&mut r.start)
                    .speed(0.1)
                    .suffix(" s")
                    .fixed_decimals(2),
            )
            .changed();
        if ui.small_button("set").clicked() {
            r.start = t;
            app.dirty = true;
        }
        ui.label("out");
        app.dirty |= ui
            .add(
                egui::DragValue::new(&mut r.end)
                    .speed(0.1)
                    .suffix(" s")
                    .fixed_decimals(2),
            )
            .changed();
        if ui.small_button("set").clicked() {
            r.end = t;
            app.dirty = true;
        }
        if ui.small_button("all").clicked() {
            r.start = 0.0;
            r.end = dur;
            app.dirty = true;
        }
    });
    ui.label("Keys: I / O set in and out at the playhead.");
    ui.add_space(10.0);

    let can = app.export.is_none() && app.project.output.path.is_some() && dur > 0.0;
    ui.add_enabled_ui(can, |ui| {
        if ui.button(RichText::new("Start export").strong()).clicked() {
            app.start_export();
        }
    });
    if let Some(job) = &app.export {
        ui.add(egui::ProgressBar::new(job.progress()).show_percentage());
        ui.label(job.status());
    }
}
