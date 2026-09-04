use crate::engine::{Clock, Mode, Quality, StreamSet};
use crate::export_job::ExportJob;
use crate::gpu::{Compositor, Target};
use crate::jobs::{JobHub, JobResult, WavData};
use crate::panels::{self, Tab};
use crate::preview;
use crate::timeline::Timeline;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use trio_core::discover::discover_shoot;
use trio_core::sync::Placement;
use trio_core::Grade;
use trio_core::{project, Orientation, Project};
use trio_media::ffmpeg::HwAccel;

pub struct App {
    pub project: Project,
    pub project_path: Option<PathBuf>,
    pub dirty: bool,

    pub comp: Arc<Compositor>,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    pub preview: Target,
    pub preview_tex: egui::TextureId,
    preview_uploaded: [u64; 3],
    preview_orientation: Orientation,

    pub streams: StreamSet,
    pub clock: Clock,
    last_advanced: Option<f64>,
    pub hwaccel: HwAccel,
    pub hwaccel_checked: bool,
    pub preview_max_edge: u32,

    pub wav: Option<WavData>,
    pub jobs: JobHub,
    pub tab: Tab,
    pub selected_camera: usize,
    pub timeline: Timeline,
    pub status: String,
    pub error: Option<String>,
    pub export: Option<ExportJob>,
    pub syncing: bool,
    pub sync_progress: (usize, usize),
    pub grading: bool,
    pub grade_progress: (usize, usize),
    /// What the last auto grade produced, so the Grade tab can undo edits.
    pub auto_grades: Option<[Grade; 3]>,
    /// Preview without any grade, toggled on the Grade tab.
    pub show_original: bool,
    sync_unmatched: Vec<String>,
    /// Folder scans still running for an "Open folder" import.
    pending_scans: usize,
    /// Start auto-sync as soon as the scans and the WAV have arrived.
    auto_sync: bool,
    pub ffmpeg_ok: bool,
    /// Startup request: seek here (and play) once the WAV has loaded.
    startup: Option<(f64, bool)>,
    /// `--export`: start the export as soon as the project is loaded.
    startup_export: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        open: Option<PathBuf>,
        start_at: Option<f64>,
        autoplay: bool,
        autoexport: bool,
    ) -> Self {
        let rs = cc.wgpu_render_state.as_ref().expect("wgpu render state");
        let comp = Arc::new(Compositor::new(rs.device.clone(), rs.queue.clone()));
        let preview = comp.create_target(1280, 720);
        let preview_tex = rs.renderer.write().register_native_texture(
            &rs.device,
            &preview.display_view,
            wgpu::FilterMode::Linear,
        );

        let (ffmpeg_ok, status) = match trio_media::ffmpeg::check_ffmpeg() {
            Ok(v) => (true, v),
            Err(e) => (false, format!("{e:#}")),
        };

        let mut app = Self {
            project: Project::default(),
            project_path: None,
            dirty: false,
            comp,
            renderer: rs.renderer.clone(),
            preview,
            preview_tex,
            preview_uploaded: [u64::MAX; 3],
            preview_orientation: Orientation::Horizontal,
            streams: StreamSet::new(Quality::Preview { max_edge: 1280 }, HwAccel::None, 30.0),
            clock: Clock::new(),
            last_advanced: None,
            hwaccel: HwAccel::None,
            hwaccel_checked: false,
            preview_max_edge: 1280,
            wav: None,
            jobs: JobHub::new(cc.egui_ctx.clone()),
            tab: Tab::Import,
            selected_camera: 0,
            timeline: Timeline::default(),
            status,
            error: None,
            export: None,
            syncing: false,
            sync_progress: (0, 0),
            grading: false,
            grade_progress: (0, 0),
            auto_grades: None,
            show_original: false,
            sync_unmatched: Vec::new(),
            pending_scans: 0,
            auto_sync: false,
            ffmpeg_ok,
            startup: start_at.map(|t| (t, autoplay)).or(if autoplay {
                Some((0.0, true))
            } else {
                None
            }),
            startup_export: autoexport,
        };
        if let Some(p) = open {
            if p.is_dir() {
                app.open_folder(p);
            } else {
                app.open_project(p);
            }
        }
        app
    }

    // ---- project lifecycle -------------------------------------------------

    pub fn new_project(&mut self) {
        self.project = Project::default();
        self.project_path = None;
        self.wav = None;
        if let Some(p) = &self.clock.player {
            p.set_pcm(Arc::new(Vec::new()));
        }
        self.clock.pause();
        self.clock.seek(0.0);
        self.reset_streams();
        self.dirty = false;
        self.pending_scans = 0;
        self.auto_sync = false;
        self.auto_grades = None;
        self.show_original = false;
    }

    /// One folder holds everything: a subfolder per camera and the master
    /// audio file. Scans the cameras, loads the audio and then syncs. When
    /// the folder already contains a project file, that is opened instead.
    pub fn open_folder(&mut self, root: PathBuf) {
        if let Some(existing) = project_in_folder(&root) {
            let name = existing_name(&existing);
            self.open_project(existing);
            self.status = format!("Opened existing project {name}");
            return;
        }
        let shoot = match discover_shoot(&root) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("{}: {e:#}", root.display()));
                return;
            }
        };
        if shoot.cameras.is_empty() {
            self.error = Some(format!(
                "No camera folders found in {}.\nExpected subfolders with video files.",
                root.display()
            ));
            return;
        }
        self.new_project();
        let stem = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "band".into());
        self.project_path = Some(root.join(format!("{stem}.trio.json")));
        self.dirty = true;
        self.timeline.reset_view();

        for (cam, folder) in shoot.cameras.iter().enumerate() {
            if let Some(name) = folder.file_name() {
                self.project.cameras[cam].name = name.to_string_lossy().into_owned();
            }
            self.jobs.scan(cam, folder.clone());
        }
        self.pending_scans = shoot.cameras.len();

        let mut notes = Vec::new();
        if shoot.cameras.len() < 3 {
            notes.push(format!("only {} camera folder(s)", shoot.cameras.len()));
        }
        if !shoot.skipped_cameras.is_empty() {
            notes.push(format!(
                "ignored extra folder(s) {}",
                shoot
                    .skipped_cameras
                    .iter()
                    .map(|p| existing_name(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        match shoot.wav.clone() {
            Some(w) => {
                if !shoot.other_audio.is_empty() {
                    notes.push(format!("using {} as master audio", existing_name(&w)));
                }
                self.import_wav(w);
            }
            None => notes.push("no audio file in the folder, sync skipped".into()),
        }
        self.status = if notes.is_empty() {
            format!("Opened {}: scanning cameras…", root.display())
        } else {
            format!("Opened {}: {}", root.display(), notes.join("; "))
        };
        self.tab = Tab::Layout;
    }

    /// Scans one camera folder picked by hand and syncs its clips once the
    /// WAV is there.
    pub fn import_camera(&mut self, cam: usize, folder: PathBuf) {
        self.jobs.scan(cam, folder);
        self.status = "Scanning…".into();
        self.auto_sync = true;
    }

    /// Loads a master WAV picked by hand and syncs the clips to it.
    pub fn import_wav(&mut self, path: PathBuf) {
        self.request_wav(path);
        self.auto_sync = true;
    }

    /// Runs the deferred auto-sync once every scan and the WAV are in.
    fn maybe_auto_sync(&mut self) {
        if self.auto_sync && self.pending_scans == 0 && self.wav.is_some() {
            self.auto_sync = false;
            self.start_sync_all();
        }
    }

    pub fn open_project(&mut self, path: PathBuf) {
        match project::load(&path) {
            Ok(p) => {
                self.new_project();
                self.project = p;
                self.project_path = Some(path);
                self.timeline.reset_view();
                if let Some(w) = self.project.wav.clone() {
                    self.request_wav(w);
                }
                self.ensure_hwaccel();
                self.tab = Tab::Layout;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn save_project(&mut self, path: Option<PathBuf>) {
        let Some(path) = path.or_else(|| self.project_path.clone()) else {
            return self.save_project_as();
        };
        match project::save(&self.project, &path) {
            Ok(()) => {
                self.project_path = Some(path);
                self.dirty = false;
                self.status = "Project saved".into();
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn save_project_as(&mut self) {
        let dialog = rfd::FileDialog::new()
            .add_filter("trio project", &["json"])
            .set_file_name("band.trio.json");
        if let Some(p) = dialog.save_file() {
            self.save_project(Some(p));
        }
    }

    pub fn request_wav(&mut self, path: PathBuf) {
        self.project.wav = Some(path.clone());
        let rate = self
            .clock
            .player
            .as_ref()
            .map(|p| (p.sample_rate(), p.channels()));
        self.jobs.load_wav(path, rate);
        self.status = "Loading WAV…".into();
    }

    pub fn ensure_hwaccel(&mut self) {
        if self.hwaccel_checked {
            return;
        }
        if let Some(c) = self
            .project
            .cameras
            .iter()
            .flat_map(|c| c.clips.first())
            .next()
        {
            self.hwaccel_checked = true;
            self.jobs.detect_hwaccel(c.path.clone());
        }
    }

    pub fn start_sync_all(&mut self) {
        let Some(wav) = &self.wav else {
            self.error = Some("Load the WAV first".into());
            return;
        };
        let total: usize = self.project.cameras.iter().map(|c| c.clips.len()).sum();
        if total == 0 {
            return;
        }
        self.syncing = true;
        self.sync_progress = (0, total);
        self.status = format!("Syncing 0/{total} clips…");
        self.jobs
            .sync(wav.mono8k.clone(), self.project.cameras.clone());
    }

    /// Measure every camera and set matching grades. Runs after auto-sync
    /// while the grades are still untouched, and from the Grade tab.
    pub fn start_auto_grade(&mut self) {
        if self.grading {
            return;
        }
        let total = trio_media::grade::sample_total(&self.project);
        if total == 0 {
            self.error = Some("Nothing to grade: no clips on the timeline".into());
            return;
        }
        self.grading = true;
        self.grade_progress = (0, total);
        self.status = format!("Auto grading 0/{total} frames…");
        self.jobs.auto_grade(self.project.clone(), self.hwaccel);
    }

    fn maybe_auto_grade(&mut self) {
        let untouched = self
            .project
            .cameras
            .iter()
            .all(|c| c.grade == Grade::default());
        if untouched && trio_media::grade::sample_total(&self.project) > 0 {
            self.start_auto_grade();
        }
    }

    pub fn duration(&self) -> f64 {
        self.project.duration(self.wav.as_ref().map(|w| w.duration))
    }

    pub fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration().max(0.0));
        self.clock.seek(t);
    }

    pub fn toggle_play(&mut self) {
        tracing::debug!(
            "toggle play: playing={} t={:.3}",
            self.clock.is_playing(),
            self.clock.time()
        );
        if self.clock.is_playing() {
            self.clock.pause();
        } else {
            if self.clock.time() >= self.duration() - 0.01 {
                self.clock.seek(0.0);
            }
            self.clock.play();
        }
    }

    /// Kick off the export of the in/out range to the configured output file.
    pub fn start_export(&mut self) {
        self.clock.pause();
        let (w, h) = self.project.output_size();
        let duration = (self.project.range.end - self.project.range.start).max(0.0);
        match ExportJob::start(&self.comp, &self.project, self.hwaccel, duration) {
            Ok(job) => {
                self.status = format!("Exporting {w}x{h}…");
                self.export = Some(job);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn apply_job(&mut self, r: JobResult) {
        match r {
            JobResult::Scanned {
                cam,
                folder,
                result,
            } => match result {
                Ok(clips) => {
                    self.status = format!("{}: {} clips", folder.display(), clips.len());
                    self.project.cameras[cam].folder = Some(folder);
                    self.project.cameras[cam].clips = clips;
                    self.dirty = true;
                    self.reset_streams();
                    self.timeline.reset_view();
                    self.ensure_hwaccel();
                    self.pending_scans = self.pending_scans.saturating_sub(1);
                    self.maybe_auto_sync();
                }
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    self.pending_scans = self.pending_scans.saturating_sub(1);
                    self.maybe_auto_sync();
                }
            },
            JobResult::WavLoaded(result) => match result {
                Ok(w) => {
                    if let Some(p) = &self.clock.player {
                        p.set_pcm(w.playback.clone());
                    }
                    self.status = format!("WAV loaded: {:.1}s", w.duration);
                    self.wav = Some(w);
                    self.timeline.reset_view();
                    self.dirty = true;
                    self.maybe_auto_sync();
                }
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    self.auto_sync = false;
                }
            },
            JobResult::SyncProgress => {
                self.sync_progress.0 += 1;
                self.status = format!(
                    "Syncing {}/{} clips…",
                    self.sync_progress.0, self.sync_progress.1
                );
            }
            JobResult::Synced { cam, index, result } => {
                if let Some(clip) = self.project.cameras[cam].clips.get_mut(index) {
                    clip.offset = result.offset;
                    clip.sync_confidence = Some(result.confidence);
                    if result.placement != Placement::Audio {
                        self.sync_unmatched.push(clip.file_name().to_string());
                    }
                    self.dirty = true;
                }
            }
            JobResult::SyncFinished => {
                self.syncing = false;
                self.status = if self.sync_unmatched.is_empty() {
                    "Sync finished".into()
                } else {
                    format!(
                        "Sync finished; no audio match for {} (placed by timestamp)",
                        self.sync_unmatched.join(", ")
                    )
                };
                self.sync_unmatched.clear();
                self.project_changed();
                self.maybe_auto_grade();
            }
            JobResult::GradeProgress => {
                self.grade_progress.0 += 1;
                self.status = format!(
                    "Auto grading {}/{} frames…",
                    self.grade_progress.0, self.grade_progress.1
                );
            }
            JobResult::Graded(result) => {
                self.grading = false;
                match result {
                    Ok(grades) => {
                        let mut auto = [Grade::default(); 3];
                        for ((cam, slot), g) in self
                            .project
                            .cameras
                            .iter_mut()
                            .zip(auto.iter_mut())
                            .zip(grades)
                        {
                            cam.grade = g;
                            *slot = g;
                        }
                        self.auto_grades = Some(auto);
                        self.dirty = true;
                        self.status = "Cameras matched; fine-tune on the Grade tab".into();
                    }
                    Err(e) => {
                        self.status = "Auto grade failed".into();
                        self.error = Some(format!("Auto grade: {e:#}"));
                    }
                }
            }
            JobResult::HwAccel(h) => {
                self.hwaccel = h;
                self.status = format!("Decoding: {}", h.label());
                self.rebuild_streams();
            }
        }
    }

    pub fn rebuild_streams(&mut self) {
        self.streams = StreamSet::new(
            Quality::Preview {
                max_edge: self.preview_max_edge,
            },
            self.hwaccel,
            self.project.output.fps,
        );
        self.preview_uploaded = [u64::MAX; 3];
        self.last_advanced = None;
    }

    /// Drop all preview decoders; they restart at the current time on the next frame.
    pub fn reset_streams(&mut self) {
        self.streams.reset();
        self.last_advanced = None;
    }

    /// Clip offsets changed: re-evaluate every camera on the next frame. Only
    /// decoders whose position no longer fits restart; the rest keep running.
    pub fn project_changed(&mut self) {
        self.dirty = true;
        self.last_advanced = None;
    }

    fn ensure_preview_target(&mut self) {
        let o = self.project.layout.orientation();
        if o != self.preview_orientation {
            let (w, h) = match o {
                Orientation::Horizontal => (1280, 720),
                Orientation::Vertical => (720, 1280),
            };
            self.preview = self.comp.create_target(w, h);
            self.renderer.write().update_egui_texture_from_wgpu_texture(
                &self.comp.device,
                &self.preview.display_view,
                wgpu::FilterMode::Linear,
                self.preview_tex,
            );
            self.preview_orientation = o;
            self.preview_uploaded = [u64::MAX; 3];
        }
    }

    fn tick_preview(&mut self, ctx: &egui::Context) {
        if (self.streams.fps - self.project.output.fps).abs() > 1e-6 {
            self.rebuild_streams();
        }
        let dur = self.duration();
        self.clock.tick();
        let mut t = self.clock.time();
        if self.clock.is_playing() && t >= dur && dur > 0.0 {
            self.clock.pause();
            self.clock.seek(dur);
            t = dur;
        }
        let moved = self
            .last_advanced
            .map(|l| (l - t).abs() > 1e-9)
            .unwrap_or(true);
        if moved || self.streams.busy() || self.streams_have_pending_frames() {
            self.streams.advance(&self.project, t, Mode::Live);
            self.last_advanced = Some(t);
        }
        for cam in 0..3 {
            let g = self.streams.generation(cam);
            if self.preview_uploaded[cam] != g {
                self.preview_uploaded[cam] = g;
                match self.streams.current(cam) {
                    Some(f) => self
                        .comp
                        .upload(&mut self.preview, cam, f.width, f.height, &f.rgba),
                    None => self.comp.clear_source(&mut self.preview, cam),
                }
            }
        }
        if self.show_original {
            let mut ungraded = self.project.clone();
            for c in ungraded.cameras.iter_mut() {
                c.grade = Grade::default();
            }
            self.comp.render(&self.preview, &ungraded);
        } else {
            self.comp.render(&self.preview, &self.project);
        }
        if self.clock.is_playing() || self.streams.busy() {
            ctx.request_repaint();
        } else if self.streams_have_pending_frames() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    /// Paused but a decoder may still deliver the frame for the current time.
    fn streams_have_pending_frames(&self) -> bool {
        self.streams.awaiting_frame()
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Typing into a text field owns the keyboard. Any other focused
        // widget (a button or slider reached with Tab, or focus left behind
        // by a dialog) must not swallow the transport keys, so those take
        // the focus away instead of being dropped silently.
        if ctx.text_edit_focused() {
            return;
        }
        const TRANSPORT: [egui::Key; 7] = [
            egui::Key::Space,
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
            egui::Key::Home,
            egui::Key::End,
            egui::Key::I,
            egui::Key::O,
        ];
        let used = ctx.input(|i| {
            TRANSPORT.iter().any(|k| i.key_pressed(*k))
                || (i.modifiers.command && i.key_pressed(egui::Key::S))
        });
        if !used {
            return;
        }
        if let Some(id) = ctx.memory(|m| m.focused()) {
            tracing::debug!("transport key takes focus from widget {id:?}");
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        let frame = 1.0 / self.project.output.fps;
        let t = self.clock.time();
        let dur = self.duration();
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                self.toggle_play();
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.seek(t - if i.modifiers.shift { 1.0 } else { frame });
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                self.seek(t + if i.modifiers.shift { 1.0 } else { frame });
            }
            if i.key_pressed(egui::Key::Home) {
                self.seek(0.0);
            }
            if i.key_pressed(egui::Key::End) {
                self.seek(dur);
            }
            if i.key_pressed(egui::Key::I) {
                self.project.range.start = t.min(self.project.range.end.max(t));
                self.dirty = true;
            }
            if i.key_pressed(egui::Key::O) {
                self.project.range.end = t;
                self.dirty = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::S) {
                self.save_project(None);
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        for r in self.jobs.poll() {
            self.apply_job(r);
        }
        if self.tab != Tab::Grade {
            self.show_original = false;
        }
        if let Some((t, play)) = self.startup {
            if self.wav.is_some() || self.project.wav.is_none() {
                self.startup = None;
                self.seek(t);
                if play {
                    self.clock.play();
                }
            }
        }
        if self.startup_export && (self.wav.is_some() || self.project.wav.is_none()) {
            self.startup_export = false;
            self.start_export();
        }
        self.handle_keys(ctx);
        self.ensure_preview_target();

        if let Some(job) = self.export.as_mut() {
            let comp = self.comp.clone();
            if job.step(&comp, Duration::from_millis(40)) {
                self.status = match &job.error {
                    Some(e) => format!("Export failed: {e}"),
                    None => format!("Export finished: {}", job.out_path.display()),
                };
                if let Some(e) = job.error.clone() {
                    self.error = Some(e);
                }
                self.export = None;
            }
            ctx.request_repaint();
        } else {
            self.tick_preview(ctx);
        }
        if self.jobs.running > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        panels::menu_bar(self, root);
        panels::side_panel(self, root);
        panels::status_bar(self, root);
        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(190.0)
            .show(root, |ui| {
                let t = self.clock.time();
                let dur = self.duration();
                let wav = self.wav.as_ref();
                let mut changed = false;
                if let Some(seek) =
                    self.timeline
                        .show(ui, &mut self.project, wav, t, dur, &mut changed)
                {
                    self.seek(seek);
                }
                if changed {
                    self.project_changed();
                }
            });
        egui::CentralPanel::default().show(root, |ui| preview::show(self, ui));

        if let Some(err) = self.error.clone() {
            egui::Window::new("Error")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(&err);
                    if ui.button("OK").clicked() {
                        self.error = None;
                    }
                });
        }
    }
}

/// The project file an earlier "Open folder" left behind, if any.
fn project_in_folder(root: &std::path::Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".trio.json"))
                    .unwrap_or(false)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

fn existing_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
