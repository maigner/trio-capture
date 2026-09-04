# trio-capture

A fast, non-destructive 3-camera composer for band recordings.

You give it three folders (one per phone camera, any number of clips each) and
one WAV from the audio recorder. It syncs every clip to the WAV by matching
audio, lets you arrange the three perspectives in a horizontal (16:9) or
vertical (9:16) layout, zoom and pan each slot, color grade each camera while
looking at the whole composition, and exports one video with the WAV as the
soundtrack. Preview and export run the same GPU shader, so what you see is
what you get.

Runs on Linux and macOS. See [docs/PLAN.md](docs/PLAN.md) for the design.

## Requirements

- `ffmpeg` and `ffprobe` on `PATH` (Linux: `apt install ffmpeg`, macOS: `brew install ffmpeg`).
- A GPU with Vulkan (Linux) or Metal (macOS).
- Rust 1.92 or newer to build.
- Linux only: `libasound2-dev` for audio playback. If it is missing, the
  repo ships a small pkg-config shim (`tools/alsa-shim`) that links against
  the system's runtime libasound so the build still works.

Hardware decoding is detected automatically (VAAPI on Linux, VideoToolbox on
macOS) and falls back to software decoding.

## Build and run

```sh
cargo build --release
./target/release/trio-capture                 # empty project
./target/release/trio-capture band.trio.json  # open a project
```

## Workflow

1. **Import** tab: open the shoot folder (one subfolder per camera and the
   master WAV next to them), or pick each folder and the WAV by hand. Every
   video file in a folder becomes a clip, ordered by its recording time.
2. Sync runs by itself as soon as the clips and the WAV are loaded. Each
   clip's own audio is cut into 15 s chunks that are matched against the WAV;
   the offset most chunks agree on wins, and the percentage shown on the
   timeline is the share of chunks that agree. Clips may start before the WAV
   (negative offset) or run past its end. The clips of one camera are then
   arranged so they never overlap; a clip whose audio matches nothing is
   placed from its recording timestamp next to a matched sibling and shown at
   0 %. Drag clips on the timeline to correct an offset (Shift+drag moves all
   clips of a camera together). The app then switches to the Layout tab.
3. **Layout** tab: choose horizontal or vertical and a preset. The layout is
   drawn large; click a slot to move it on to the next camera, or use the
   camera buttons under it. In the preview, drag inside a slot to move the
   picture and scroll to zoom; *Reset framing* undoes that.
4. **Grade** tab: the cameras are matched to each other automatically after
   sync. Click a slot to pick its camera, then move the plain sliders
   (brightness, contrast, colour, warmth, tint) until it looks right;
   *Show original* compares with the recording, *Back to automatic* drops
   your changes, and double-clicking a slider resets just that one. *More*
   holds shadows, mid-tones and highlights. The grade follows the camera into
   every slot it occupies.
5. **Export** tab: resolution, frame rate, codec, output file, range, go.
   Export runs inside the app with a progress bar and can be cancelled.

Keyboard: Space play/pause, Left/Right one frame, Shift+Left/Right one
second, Home/End, I and O set the export range at the playhead, Ctrl+S saves.
Timeline: Ctrl+scroll, pinch or the +/− keys zoom around the pointer or
playhead, 0 or *Fit* shows the whole recording, Shift+scroll pans, click the
ruler to seek. Clips sit where the automatic sync put them and cannot be
dragged.

## Headless commands

Useful for scripting and for checking a shoot without opening the UI:

```sh
trio-capture new band.trio.json --cam cam1/ --cam cam2/ --cam cam3/ --wav master.wav
trio-capture sync band.trio.json          # prints offsets and confidences
trio-capture export band.trio.json --out band.mp4
trio-capture probe clip.mp4               # what ffprobe tells us about a clip
```

## Test footage

`tools/make-testdata.py` renders a synthetic three-camera shoot into
`testdata/generated/` (different resolutions and frame rates, two clips per
camera with gaps, audio cut from a master WAV at known offsets, burned-in
master time). `expected.json` lists the true offsets so the sync can be
verified:

```sh
python3 tools/make-testdata.py
cargo run -- new testdata/generated/test.trio.json \
  --cam testdata/generated/cam1 --cam testdata/generated/cam2 \
  --cam testdata/generated/cam3 --wav testdata/generated/master.wav
cargo run -- sync testdata/generated/test.trio.json
```

## Shooting checklist

- All phones: SDR, fixed 30 fps (or all 24/25), same resolution, locked
  exposure and white balance, HDR and "auto frame rate" off.
- Clap or count in on camera at the start so manual sync is always possible.
- Keep the phones recording continuously where possible.

## Project layout

```
crates/trio-core    project model, ffprobe wrapper, folder discovery, layouts, sync math
crates/trio-media   ffmpeg decode streams, PCM extraction, cpal playback clock, encoder
crates/trio-app     egui/wgpu application, compositing shader, timeline, headless CLI
```

Project files are plain JSON (`*.trio.json`) and reference the media by
path; nothing is ever written into the camera folders.
