//! Headless commands: build a project from folders, sync it, export it.

use crate::engine::{Mode, Quality, StreamSet};
use crate::export_job::{export_spec_for, EXPORT_MAX_EDGE};
use crate::gpu::Compositor;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use trio_core::discover::{discover_shoot, scan_folder};
use trio_core::sync::{Master, Placement, SYNC_RATE};
use trio_core::{project, Project};
use trio_media::audio::decode_pcm;
use trio_media::export::Encoder;
use trio_media::ffmpeg::detect_hwaccel;
use trio_media::sync::sync_cameras;

pub fn run(args: &[String]) -> Result<()> {
    match args[0].as_str() {
        "new" => cmd_new(&args[1..]),
        "sync" => cmd_sync(&args[1..]),
        "export" => cmd_export(&args[1..]),
        "grade" => cmd_grade(&args[1..]),
        "probe" => {
            for p in &args[1..] {
                println!("{:#?}", trio_core::probe::probe_clip(Path::new(p))?);
            }
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn usage() -> anyhow::Error {
    anyhow!(
        "usage:\n  trio-capture new <out.trio.json> --root SHOOT_DIR\n  \
         trio-capture new <out.trio.json> --cam DIR --cam DIR --cam DIR [--wav FILE]\n  \
         trio-capture sync <project.trio.json>\n  \
         trio-capture export <project.trio.json> [--out FILE]\n  \
         trio-capture grade <project.trio.json>\n  \
         trio-capture probe FILE..."
    )
}

fn cmd_new(args: &[String]) -> Result<()> {
    let out = args.first().ok_or_else(usage)?;
    let mut project = Project::default();
    let mut cam = 0;
    let mut i = 1;
    let mut add_cam = |project: &mut Project, dir: PathBuf| -> Result<()> {
        if cam >= project.cameras.len() {
            return Err(anyhow!("only 3 cameras supported"));
        }
        let clips = scan_folder(&dir).with_context(|| format!("scanning {}", dir.display()))?;
        println!(
            "cam {}: {} clips in {}",
            cam + 1,
            clips.len(),
            dir.display()
        );
        if let Some(name) = dir.file_name() {
            project.cameras[cam].name = name.to_string_lossy().into_owned();
        }
        project.cameras[cam].folder = Some(dir);
        project.cameras[cam].clips = clips;
        cam += 1;
        Ok(())
    };
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                let root = PathBuf::from(args.get(i + 1).ok_or_else(usage)?);
                let shoot = discover_shoot(&root)
                    .with_context(|| format!("looking into {}", root.display()))?;
                if shoot.cameras.is_empty() {
                    return Err(anyhow!("no camera folders in {}", root.display()));
                }
                for dir in shoot.cameras {
                    add_cam(&mut project, dir)?;
                }
                for dir in &shoot.skipped_cameras {
                    println!("ignored extra folder {}", dir.display());
                }
                match &shoot.wav {
                    Some(w) => println!("audio: {}", w.display()),
                    None => println!("audio: none found in {}", root.display()),
                }
                project.wav = shoot.wav;
                i += 2;
            }
            "--cam" => {
                let dir = PathBuf::from(args.get(i + 1).ok_or_else(usage)?);
                add_cam(&mut project, dir)?;
                i += 2;
            }
            "--wav" => {
                project.wav = Some(PathBuf::from(args.get(i + 1).ok_or_else(usage)?));
                i += 2;
            }
            _ => return Err(usage()),
        }
    }
    project::save(&project, Path::new(out))?;
    println!("wrote {out}");
    Ok(())
}

fn cmd_sync(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().ok_or_else(usage)?);
    let mut proj = project::load(&path)?;
    let wav = proj
        .wav
        .clone()
        .ok_or_else(|| anyhow!("project has no WAV"))?;
    let master = Master::new(std::sync::Arc::new(decode_pcm(&wav, SYNC_RATE, 1)?));
    let results = sync_cameras(&master, &proj.cameras, &|_, _| {});
    for (cam, res) in proj.cameras.iter_mut().zip(&results) {
        for (clip, r) in cam.clips.iter_mut().zip(res) {
            let note = match r.placement {
                Placement::Audio => "",
                Placement::Timestamp => "  (no audio match, placed by timestamp)",
                Placement::Unknown => "  (no audio match, offset unchanged)",
            };
            println!(
                "{:<28} offset {:>9.3}s  confidence {:.2}{note}",
                clip.file_name(),
                r.offset,
                r.confidence
            );
            clip.offset = r.offset;
            clip.sync_confidence = Some(r.confidence);
        }
    }
    project::save(&proj, &path)?;
    Ok(())
}

/// Measure the cameras and store matching grades in the project.
fn cmd_grade(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().ok_or_else(usage)?);
    let mut proj = project::load(&path)?;
    let hwaccel = proj
        .cameras
        .iter()
        .flat_map(|c| c.clips.first())
        .next()
        .map(|c| detect_hwaccel(&c.path))
        .unwrap_or_default_none();
    let started = std::time::Instant::now();
    let grades = trio_media::grade::auto_grade(&proj, hwaccel, &|| {})?;
    for (cam, g) in proj.cameras.iter_mut().zip(grades) {
        println!(
            "{:<12} exposure {:+.2}  contrast {:.2}  saturation {:.2}  temperature {:+.2}  tint {:+.2}",
            cam.name, g.exposure, g.contrast, g.saturation, g.temperature, g.tint
        );
        cam.grade = g;
    }
    println!("analysed in {:.1}s", started.elapsed().as_secs_f64());
    project::save(&proj, &path)?;
    Ok(())
}

fn cmd_export(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().ok_or_else(usage)?);
    let mut proj = project::load(&path)?;
    if let Some(i) = args.iter().position(|a| a == "--out") {
        proj.output.path = Some(PathBuf::from(args.get(i + 1).ok_or_else(usage)?));
    }
    let out = proj
        .output
        .path
        .clone()
        .ok_or_else(|| anyhow!("no output path (use --out)"))?;

    tracing::info!("creating headless GPU device");
    let (device, queue) = headless_device()?;
    tracing::info!("device ready");
    let comp = Compositor::new(device, queue);
    let (w, h) = proj.output_size();
    let mut target = comp.create_target(w, h);

    let wav_duration = proj.wav.as_ref().map(|p| {
        trio_core::probe::probe_clip(p)
            .map(|c| c.duration)
            .unwrap_or(0.0)
    });
    let duration = if proj.range.end > proj.range.start {
        proj.range.end - proj.range.start
    } else {
        proj.duration(wav_duration) - proj.range.start
    };
    let hwaccel = proj
        .cameras
        .iter()
        .flat_map(|c| c.clips.first())
        .next()
        .map(|c| detect_hwaccel(&c.path))
        .unwrap_or_default_none();
    let spec = export_spec_for(&proj, &out, duration, hwaccel);
    println!(
        "export {}x{} @ {} fps, {:.2}s, decode: {}, encode: {}",
        w,
        h,
        spec.fps,
        duration,
        hwaccel.label(),
        spec.codec.label()
    );

    let mut encoder = Encoder::start(&spec)?;
    let mut streams = StreamSet::new(
        Quality::Full {
            max_edge: EXPORT_MAX_EDGE,
        },
        hwaccel,
        spec.fps,
    );
    let total = (duration * spec.fps).round() as u64;
    let mut uploaded = [u64::MAX; 3];
    let started = std::time::Instant::now();
    for f in 0..total {
        let t = proj.range.start + f as f64 / spec.fps;
        tracing::debug!("frame {f} t={t:.3}: advancing streams");
        streams.advance(&proj, t, Mode::Exact);
        tracing::debug!("frame {f}: uploading");
        for cam in 0..3 {
            if uploaded[cam] != streams.generation(cam) {
                uploaded[cam] = streams.generation(cam);
                match streams.current(cam) {
                    Some(fr) => comp.upload(&mut target, cam, fr.width, fr.height, &fr.rgba),
                    None => comp.clear_source(&mut target, cam),
                }
            }
        }
        comp.render(&target, &proj);
        tracing::debug!("frame {f}: readback");
        let bytes = comp.readback(&mut target);
        tracing::debug!("frame {f}: encode");
        encoder.write_frame(&bytes)?;
        if f % 30 == 0 {
            eprint!(
                "\r{f}/{total} frames ({:.1} fps)",
                f as f64 / started.elapsed().as_secs_f64().max(1e-3)
            );
        }
    }
    encoder.finish()?;
    eprintln!(
        "\rdone: {} frames in {:.1}s -> {}",
        total,
        started.elapsed().as_secs_f64(),
        out.display()
    );
    Ok(())
}

trait OrNone {
    fn unwrap_or_default_none(self) -> trio_media::ffmpeg::HwAccel;
}
impl OrNone for Option<trio_media::ffmpeg::HwAccel> {
    fn unwrap_or_default_none(self) -> trio_media::ffmpeg::HwAccel {
        self.unwrap_or(trio_media::ffmpeg::HwAccel::None)
    }
}

fn headless_device() -> Result<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster_block(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .context("no GPU adapter")?;
    let (device, queue) = pollster_block(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("trio headless"),
        ..Default::default()
    }))
    .context("requesting device")?;
    Ok((device, queue))
}

/// Minimal executor so the CLI does not need an async runtime.
fn pollster_block<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
