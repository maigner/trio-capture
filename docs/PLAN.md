# trio-capture — Implementation Plan

A fast, non-destructive 3-camera composer for band recordings.
Input: three folders (one per phone camera) plus one WAV. Output: one video
in a horizontal (16:9) or vertical (9:16) layout with the three perspectives
placed, zoomed/panned and color graded, with the WAV as the soundtrack.

## 1. Requirements (from the brief)

| # | Requirement | Notes / hidden complexity |
|---|-------------|---------------------------|
| R1 | 3 input folders, one per camera, each with many files | Clips must be ordered and placed on a per-camera track with gaps allowed |
| R1b | Open one shoot folder; cameras and WAV are found by themselves and sync runs | `discover_shoot`: subfolders with video files are cameras (name order, "cam" names win when there are more than three), the audio file in the root is the master (`.wav` first, then largest). GUI defers sync until scans and WAV are in |
| R2 | Horizontal and vertical output layouts | Layout presets; each preset has 3 slots |
| R3 | Choose which camera goes in which slot | Slot -> camera mapping, swappable at any time |
| R4 | Zoom and pan each video a little | Per-slot scale + offset, content is cropped to fill the slot |
| R5 | Live preview | GPU compositing; 3 concurrent decoders; scrubbing must feel instant |
| R6 | Fast | Hardware decode, proxies as fallback, all image math in one GPU shader |
| R7 | Linux and macOS | Rust + wgpu + ffmpeg. VAAPI on Linux, VideoToolbox on macOS |
| R8 | Color grade each source while seeing the composed picture | Grade is per camera, applied inside the compositing shader |
| R9 | Add a WAV as the audio track | WAV is the master clock of the timeline |
| R10 | Simple sync of the 3 camera streams to the WAV | Automatic audio cross-correlation, manual nudge as fallback |

Phone-footage realities the design must handle from day one:

- **Variable frame rate (VFR).** Android especially. Never assume a fixed
  fps; drive everything by presentation timestamps (pts) on a master clock.
- **Rotation metadata.** Phones store a display matrix instead of rotating
  pixels. Must be honored on import.
- **HDR by default.** iPhones record 10-bit HEVC HLG / Dolby Vision unless
  switched off. Needs HLG -> SDR conversion in the shader, or a consistent
  10-bit pipeline. Recommendation: shoot SDR ("Most Compatible"), but support
  HDR clips so old footage still works.
- **Many clips per camera** with stop/start gaps and file splits (Android
  splits at 4 GB). Every clip gets its own offset; gaps render black.
- **Clock drift** between devices over a long set. Usually well under 100 ms
  per hour, so v1 offers one offset per clip; per-camera speed correction is a
  later feature.

## 2. Technology decisions

| Area | Choice | Why |
|------|--------|-----|
| Language | Rust (edition 2024) | Fast, cross-platform, already installed, no runtime to ship |
| UI | `eframe`/`egui` on the `wgpu` backend | Immediate-mode UI is ideal for a tool with a big preview viewport; trivial to show GPU textures |
| GPU | `wgpu` + WGSL shaders | Vulkan on Linux, Metal on macOS, one shader source |
| Decode | Spawn the `ffmpeg` CLI per clip, RGBA frames over a pipe (decision taken in Phase 0: no libclang on the dev machine, and no native linking makes distribution trivial) | `-ss` before `-i` gives an accurate keyframe seek, the `fps` filter turns VFR into our fixed grid, hwaccel via VAAPI / VideoToolbox with GPU-side scaling. A seek costs one process restart (~100-300 ms) |
| Encode | Spawn the `ffmpeg` CLI, feed rawvideo over stdin | Simple, robust, gives every encoder for free (`hevc_vaapi`, `h264_videotoolbox`, `libx264`) |
| Audio analysis | ffmpeg for PCM extraction (WAV and clip audio alike), `rustfft` | Cross-correlation for sync, waveform display |
| Audio playback | `cpal` | Preview plays the WAV, video follows the audio clock |
| Project file | `serde` + JSON (`*.trio.json`) | Human-readable, non-destructive, easy to diff |
| Packaging | AppImage on Linux, `.app` bundle via `cargo-bundle` on macOS | Later phase |

The preview and the export use the **same compositing shader**, so what you
see is exactly what you get.

## 3. Architecture

```
+-----------------------------------------------------------------------+
| UI (egui)                                                             |
|  Import | Sync | Layout | Grade | Export   ...  Timeline + Preview    |
+---------------------------+-------------------------------------------+
                            | Project (single source of truth, serde)
                            v
+---------------------------+-------------------------------------------+
| Engine                                                                |
|  MasterClock (WAV time)                                               |
|  CameraTrack x3 -> Clip list (path, in, out, offset_on_master)        |
|  StreamDecoder x3 (thread each, hw decode, frame ring buffer)         |
|  Compositor (wgpu): 3 textures -> layout -> zoom/pan -> grade -> out  |
|  AudioEngine (cpal): plays WAV, publishes clock                       |
|  Exporter: Compositor -> readback -> ffmpeg stdin                     |
+-----------------------------------------------------------------------+
```

Key engine design points:

- **Master clock = WAV time.** t=0 is the first WAV sample. Every clip stores
  `offset` = master time where the clip's first frame appears.
- **Decoder per camera**, on its own thread, decoding to NV12/P010 and
  uploading into a small ring of GPU textures. Seek = flush ring, keyframe
  seek, decode forward to the target pts. Frames are selected by
  "latest pts <= master time", which is what makes VFR a non-issue.
- **One WGSL fragment shader** does per slot: sample source (YUV -> RGB,
  rotation, HLG->SDR if needed), apply zoom/pan (crop-to-fill), apply that
  camera's grade, write into the slot rectangle. Grade parameters are a
  uniform buffer, so slider changes are free.
- **Frame cache** of recently decoded frames per stream, so scrubbing back
  and forth near the playhead never re-decodes.
- **Proxies** (optional, generated in the background on import):
  1080p H.264 all-intra, used for preview when the source is 4K HEVC and
  hardware decode is unavailable. Export always uses originals.

Cargo workspace layout:

```
trio-capture/
  crates/
    trio-core/     project model, clip discovery, probing, sync math   (no GPU, unit-testable)
    trio-media/    decoders, audio extraction, exporter (ffmpeg)
    trio-gpu/      wgpu compositor, WGSL shaders, layouts
    trio-app/      egui application
  docs/
  assets/luts/
```

## 4. Data model

```rust
Project {
  wav: PathBuf,
  cameras: [Camera; 3],          // each with folder, clips, grade
  layout: LayoutId,              // e.g. H_SideBySide, H_OneBigTwoSmall, V_Stacked, V_OneBigTwoSmall
  slots: [Slot; 3],              // slot i -> { camera: 0..3, zoom: f32, pan: (f32,f32) }
  output: { width, height, fps, codec, path },
  range: { in: f64, out: f64 },  // master-time seconds
}
Camera { folder, clips: Vec<Clip>, grade: Grade, speed: f64 /* 1.0 in v1 */ }
Clip   { path, duration, fps_hint, rotation, hdr: bool, offset: f64, sync_confidence: f32 }
Grade  { exposure, contrast, saturation, temperature, tint, lift, gamma, gain, lut: Option<PathBuf> }
```

## 5. Sync design (R10)

1. Extract each clip's own audio and the WAV to mono 8 kHz PCM (ffmpeg).
2. Log-RMS envelope at 100 Hz of both, which is robust to the phone's
   compression and AGC.
3. The clip envelope is cut into 15 s chunks (10 s stride). Each chunk is
   matched against the whole WAV by normalized cross-correlation (one FFT per
   chunk; the WAV spectrum is computed once; only lags where the chunk lies
   fully inside the WAV count, partial overlaps produce bogus peaks). Chunks
   vote for the clip offset they imply; the densest cluster (±0.5 s) wins, a
   line fit through its members gives the offset at the clip start (absorbs
   clock drift), and a ±20 ms raw-PCM pass refines it. Confidence = share of
   the overlapping chunks that voted for the winner. This survives a clip
   that starts before the WAV, sections the WAV does not contain, and songs
   that appear twice (soundcheck and show), because a wrong peak only ever
   convinces a few chunks. Up to four candidates per clip are kept.
4. Per camera, clips were recorded one after another and can never overlap.
   A small dynamic programme picks one candidate per clip (in recording
   order) maximizing total confidence under that constraint, with a bonus
   when the gap between two clips agrees with their `creation_time` stamps.
   Clips without a usable candidate are placed from timestamps next to the
   nearest matched sibling and flagged at 0 %. Phones that stamp the end of
   the recording instead of the start are detected from a gap shorter than
   the clip.
5. UI shows each clip on its camera track, colored by confidence. Low
   confidence clips get a warning; the user can drag them or type an offset,
   and a "nudge by frame" control refines. Playing all three plus the WAV
   makes any error audible immediately.
6. Fallback for clips without usable audio: manual placement on a visible
   transient (a clap or drum hit) using the waveform overlay.

## 6. Layouts (R2, R3, R4)

Layouts are data, not code: a list of slot rectangles in normalized output
coordinates. Initial presets:

- Horizontal 16:9: three side by side; one large left + two stacked right;
  one large center + two small corners (picture-in-picture).
- Vertical 9:16: three stacked; one large top + two side by side below;
  one full-frame + two small overlays.

Each slot: camera index, `zoom` (1.0 to about 2.0), `pan` in normalized units.
Content is scaled to cover the slot, then zoom/pan shift the crop window.
Dragging inside a slot pans, scroll wheel zooms. Slot swap is a drag between
slots or a dropdown.

## 7. Grading (R8)

Per-camera controls, applied to that camera wherever it appears, while the
full composition stays on screen: exposure, contrast, saturation,
temperature/tint, lift/gamma/gain, optional 3D LUT (`.cube`). A "match"
helper shows the three sources' histograms side by side. All math happens in
the shader in linear light; grade changes never touch the decoders.

## 8. Export (R2, R7)

Render every output frame at the chosen fps by advancing master time, pulling
the frame with latest pts <= time from each stream, running the compositor at
output resolution, reading back, and writing rawvideo to ffmpeg's stdin. The
WAV is muxed in directly (`-i master.wav -c:a aac` or copy as PCM). Encoder
choice: `hevc_vaapi`/`h264_vaapi` on Linux, `*_videotoolbox` on macOS,
`libx264` fallback. Presets: 1080p and 4K, 16:9 and 9:16.

## 9. Phases

Each phase ends with something runnable. Suggested order keeps the risky bits
(decode, GPU, sync) early.

**Phase 0 — Skeleton (1 to 2 days)**
- Cargo workspace, eframe+wgpu window, empty viewport, GitHub Actions matrix
  for Ubuntu and macOS.
- Decision gate: confirm `ffmpeg-the-third` builds and links on both OSes.
  If not, switch to `ffmpeg-sidecar` before writing decoder code.

**Phase 1 — Import and project model (2 to 3 days)**
- Pick three folders and a WAV. Probe every clip (duration, fps, codec,
  pixel format, rotation, HDR flags, creation_time). Sort by creation_time.
- Save/load `project.trio.json`. Unit tests for discovery and ordering.

**Phase 2 — Single-stream playback (3 to 5 days)**
- Decoder thread with hw acceleration, NV12/P010 upload, YUV->RGB shader,
  rotation handling. Play, pause, seek, scrub. Frame ring + cache.
- Milestone: scrub a 4K HEVC iPhone clip smoothly; measure seek latency.

**Phase 3 — Timeline and sync (3 to 5 days)**
- WAV playback via cpal as master clock; waveform strip.
- Audio extraction, envelope, FFT cross-correlation, confidence.
- Track view with clips per camera, drag to nudge, type an offset.
- Milestone: three cameras follow the WAV in lip sync across a whole set.

**Phase 4 — Compositing, layouts, zoom/pan (3 to 4 days)**
- Three decoders live at once. Layout presets, slot assignment, per-slot
  zoom/pan with mouse. Output aspect switch H/V.
- Milestone: live composed preview at real time with three 4K sources.

**Phase 5 — Grading (2 to 3 days)**
- Grade uniforms and shader math, per-camera panel, LUT loader, histograms.
- HLG -> SDR path for HDR clips.

**Phase 6 — Export (2 to 3 days)**
- Render loop, readback, ffmpeg encoder process, progress bar, cancel.
- Verify frame accuracy: exported frame N equals preview at that time.

**Phase 7 — Speed and robustness (ongoing, 3+ days)**
- Proxy generation, prefetch during playback, decoder pooling, profiling
  with `tracing` + `puffin`.
- Targets on this machine: 3x4K HEVC preview at 30 fps, seek under 100 ms
  with hw decode, export at or above real time in 1080p.

**Phase 8 — Polish and packaging**
- Keyboard shortcuts (JKL, arrows per frame, I/O for range), recent projects,
  AppImage and macOS bundle, README with the shooting checklist (SDR, fixed
  fps, same fps on all phones).

## 10. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| libav linking on macOS / distribution size | Phase 0 gate; sidecar fallback; `brew install ffmpeg` as documented prerequisite for v1 |
| Hardware decode of 10-bit HEVC not available on a given machine | Software decode via ffmpeg threads plus proxies |
| Audio sync false positives on repetitive music | Correlate near creation_time; show confidence; manual nudge |
| Clock drift over a 60+ minute set | Per-camera `speed` field reserved; implement in Phase 7 if measurable |
| HDR footage looks washed out | HLG->SDR in shader; recommend SDR capture |
| Long clips make cross-correlation slow | 8 kHz envelope + windowed FFT keeps it under a second per clip |

## 11. Out of scope for v1

Cuts between layouts over time, transitions, titles, multi-song splitting,
audio mixing beyond the single WAV. The data model leaves room for a
"layout keyframe" list so cutting between layouts can be added later.

## 12. Shooting checklist (reduces engineering pain)

- All phones: SDR, fixed 30 fps (or all 24/25), same resolution, lock
  exposure and white balance, disable HDR and "auto frame rate".
- Clap or count in on camera at the start so manual sync is always possible.
- Keep the phones recording continuously where possible; fewer clips means
  fewer sync points.

## 13. Status (2026-09-04)

Phases 0 to 6 are implemented and verified on synthetic footage
(`tools/make-testdata.py`): import, auto-sync (offsets recovered within 1 ms
at 100 % confidence), horizontal and vertical layouts with zoom/pan,
per-camera grading, live preview with the WAV as master clock, and export
(1080p from three sources at about 55 fps with the software H.264 encoder on
the development machine). Headless `new` / `sync` / `export` commands exist
for scripting and CI.

Verified on a real two-set gig (GoPro, iPhone, Android; 10 clips, 66 min
WAV with the break between sets removed): the first version of the sync
(one global cross-correlation per clip) misplaced an Android clip that
started 163 s before the WAV on top of its successor. The chunk-vote design
above places all ten clips consistently with their timestamps (confidence
0.32-0.84, 8 s for the whole project) and enforces non-overlap per camera.
`cargo run --release -p trio-core --example syncdbg master.pcm clip.pcm`
(mono f32le at 8 kHz, `VOTES=1` for per-chunk detail) helps when a clip
still lands wrong.

Import is a single "Open folder" step: the shoot folder is inspected for
camera subfolders and the master audio file, the cameras are scanned, the
audio is loaded, and sync starts once both are in. Verified on the same gig
folder (which also contains a DAW project folder that is correctly ignored)
and on `testdata/generated`. `trio-capture new out.json --root DIR` and
`trio-capture DIR` do the same from the command line.

Open items from Phase 7 and 8: proxies for machines without hardware decode,
frame cache for scrub-back, HDR tone mapping is wired but untested on real
HLG footage, per-camera speed correction for clock drift, packaging
(AppImage / .app bundle), LUT support.

Seeking fix (2026-09-04): jumping far and pressing play left some cameras
stuck on the old frame. The engine compared the requested time against the
frame still on screen, which belonged to the previous decoder, so a new
decoder that needed more than the 120 ms restart debounce for its first
frame was killed and started again forever (the GUI log showed the same
"decoder start" line every 150 ms). The engine now tracks the position of
the current stream itself (`stream_shown`), counts decoder starts, and an
ignored regression test in `engine.rs` (`TRIO_PROJECT=path cargo test
--release -p trio-app -- --ignored`) jumps across a real project and
requires one decoder start per camera per jump. First frames after a far
jump take about 0.5 s on the gig footage.

Preview brightness fix (2026-09-04): the composite target is `Rgba8UnormSrgb`
and its sRGB view was registered with egui. egui-wgpu 0.35 expects plain
`Rgba8Unorm` textures and treats sampled texels as gamma-encoded, so it showed
the linear values and the preview was far too dark while the export, read back
as sRGB bytes, was right. `Target::display_view` is now a `Rgba8Unorm`
reinterpretation of the same texture (`view_formats`) and is the view egui
draws. Preview and export now agree in mean luma to within a few percent. Grades
tuned against the old dark preview come out too bright and need revisiting.

Auto grade (2026-09-04): once sync has finished (and the grades are still
untouched), the app measures every camera and sets matching grades; the Grade
tab has an "Auto grade all cameras" button and `trio-capture grade
<project>` does the same headless. `trio_media::grade::auto_grade` grabs
eight 320 px frames per camera (`decoder::grab_frame`, one ffmpeg call each,
in parallel) at moments all cameras cover, restricted to the part of the
frame the slot actually shows (`autograde::visible_region` mirrors the
shader's cover fit, zoom and pan). `trio_core::autograde::solve` then
derives, per camera: exposure so the mean display luma meets the cameras'
average (clamped to 0.28..0.50, with a highlight guard on the 90th
percentile), contrast from the 5..95 percentile spread (0.95..1.3),
saturation from the mean chroma (0.8..1.5), and temperature/tint that move
the linear R/G and B/G ratios halfway toward the cameras' median. Targets
come from the footage itself, so a dark gig stays dark and stage colours
survive; the half-strength balance keeps a pink-lit drummer from being
neutralised against a warm bar. `Grade::apply` is a CPU mirror of the
shader used by the unit tests. On the gig project the three cameras land
within a few percent in mean luma and the analysis takes about a second
with VAAPI.
