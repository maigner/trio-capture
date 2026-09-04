//! Keeps one decoder per camera aligned with the master timeline.

use std::time::{Duration, Instant};
use trio_core::{Clip, Project, CAMERA_COUNT};
use trio_media::decoder::{DecodeRequest, Frame, FrameStream};
use trio_media::ffmpeg::{fit_size, HwAccel};
use trio_media::player::Player;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quality {
    /// Longest edge capped, for the live preview.
    Preview { max_edge: u32 },
    /// Native size capped at `max_edge`, for export.
    Full { max_edge: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Never block; keep showing the last frame while the decoder catches up.
    Live,
    /// Block until the frame for the requested time is available.
    Exact,
}

struct CamState {
    stream: Option<FrameStream>,
    clip_index: Option<usize>,
    current: Option<Frame>,
    peek: Option<Frame>,
    /// Bumps whenever `current` changes so the GPU upload happens once.
    generation: u64,
    expected_next: f64,
    /// Time of the last frame taken from the current stream. `None` until the
    /// stream delivered, so a stale frame from an earlier position never
    /// drives the restart decision.
    stream_shown: Option<f64>,
    last_restart: Instant,
    pending_restart: bool,
    /// A live decoder has not yet delivered the frame for the last requested time.
    waiting: bool,
}

impl CamState {
    fn new() -> Self {
        Self {
            stream: None,
            clip_index: None,
            current: None,
            peek: None,
            generation: 0,
            expected_next: 0.0,
            stream_shown: None,
            last_restart: Instant::now() - Duration::from_secs(10),
            pending_restart: false,
            waiting: false,
        }
    }
    fn drop_stream(&mut self) {
        self.stream = None;
        self.peek = None;
        self.clip_index = None;
        self.stream_shown = None;
    }
    fn set_current(&mut self, f: Option<Frame>) {
        self.current = f;
        self.generation += 1;
    }
}

pub struct StreamSet {
    pub quality: Quality,
    pub hwaccel: HwAccel,
    pub fps: f64,
    /// Decoder starts so far, for diagnostics.
    pub restarts: u64,
    cams: Vec<CamState>,
}

const RESTART_DEBOUNCE: Duration = Duration::from_millis(120);
/// How far ahead of the decoder position we tolerate before re-seeking.
const MAX_AHEAD_SECONDS: f64 = 1.5;
const EXACT_TIMEOUT: Duration = Duration::from_secs(20);

impl StreamSet {
    pub fn new(quality: Quality, hwaccel: HwAccel, fps: f64) -> Self {
        Self {
            quality,
            hwaccel,
            fps,
            restarts: 0,
            cams: (0..CAMERA_COUNT).map(|_| CamState::new()).collect(),
        }
    }

    pub fn reset(&mut self) {
        for c in &mut self.cams {
            c.drop_stream();
            c.set_current(None);
        }
    }

    pub fn generation(&self, cam: usize) -> u64 {
        self.cams[cam].generation
    }

    pub fn current(&self, cam: usize) -> Option<&Frame> {
        self.cams[cam].current.as_ref()
    }

    /// True while some camera is waiting for its debounced decoder restart.
    pub fn busy(&self) -> bool {
        self.cams.iter().any(|c| c.pending_restart)
    }

    /// True while a decoder still owes the frame for the last requested time.
    pub fn awaiting_frame(&self) -> bool {
        self.cams.iter().any(|c| c.waiting)
    }

    fn decode_size(&self, clip: &Clip) -> (u32, u32) {
        match self.quality {
            Quality::Preview { max_edge } | Quality::Full { max_edge } => {
                fit_size(clip.width, clip.height, max_edge, max_edge)
            }
        }
    }

    /// Bring every camera to master time `t`.
    pub fn advance(&mut self, project: &Project, t: f64, mode: Mode) {
        let half = 0.5 / self.fps;
        for cam in 0..CAMERA_COUNT {
            let Some((idx, clip)) = project.clip_at(cam, t) else {
                let st = &mut self.cams[cam];
                st.drop_stream();
                if st.current.is_some() {
                    st.set_current(None);
                }
                st.pending_restart = false;
                st.waiting = false;
                continue;
            };
            let clip = clip.clone();
            let local = t - clip.offset;
            let (w, h) = self.decode_size(&clip);
            let st = &mut self.cams[cam];

            // Position of the current stream: its last delivered frame, or its
            // start while the first frame is still on the way. The frame on
            // screen may belong to an older stream and must not count here,
            // otherwise a decoder slower than the debounce is restarted forever.
            let shown = st.stream_shown.unwrap_or(st.expected_next);
            let needs_restart = st.stream.is_none()
                || st.clip_index != Some(idx)
                || local < shown - half - 1e-6
                || local > shown + MAX_AHEAD_SECONDS;

            if needs_restart {
                let allowed = mode == Mode::Exact || st.last_restart.elapsed() >= RESTART_DEBOUNCE;
                if !allowed {
                    st.pending_restart = true;
                    continue;
                }
                st.pending_restart = false;
                st.drop_stream();
                // Snap the start onto the frame grid so preview and export agree.
                let start = (local * self.fps).floor().max(0.0) / self.fps;
                let req = DecodeRequest {
                    path: clip.path.clone(),
                    start,
                    fps: self.fps,
                    width: w,
                    height: h,
                    hdr: clip.hdr,
                    hwaccel: self.hwaccel,
                };
                tracing::info!(
                    "cam {cam}: decoder start {} at {start:.3}s ({w}x{h})",
                    clip.file_name()
                );
                self.restarts += 1;
                match FrameStream::start(req) {
                    Ok(s) => {
                        st.stream = Some(s);
                        st.clip_index = Some(idx);
                        st.expected_next = start;
                        st.last_restart = Instant::now();
                    }
                    Err(e) => {
                        tracing::error!("decoder start failed: {e:#}");
                        st.set_current(None);
                        continue;
                    }
                }
            }

            // Pull frames up to the requested time.
            let deadline = Instant::now() + EXACT_TIMEOUT;
            loop {
                if st.peek.is_none() {
                    let Some(stream) = st.stream.as_mut() else {
                        break;
                    };
                    st.peek = match mode {
                        Mode::Live => stream.try_next(),
                        Mode::Exact => stream.next_timeout(Duration::from_millis(200)),
                    };
                    if st.peek.is_none() {
                        if stream.finished() || mode == Mode::Live || Instant::now() > deadline {
                            break;
                        }
                        continue;
                    }
                }
                let due = st
                    .peek
                    .as_ref()
                    .map(|f| f.time <= local + half)
                    .unwrap_or(false);
                if due {
                    let f = st.peek.take().unwrap();
                    st.expected_next = f.time + 1.0 / self.fps;
                    st.stream_shown = Some(f.time);
                    st.set_current(Some(f));
                } else {
                    break;
                }
            }
            let alive = st.stream.as_ref().map(|s| !s.finished()).unwrap_or(false);
            let satisfied = st.stream_shown.map(|t| t >= local - half).unwrap_or(false);
            st.waiting = alive && !satisfied;
        }
    }
}

/// Master clock: the audio player when a WAV is loaded, a wall clock otherwise.
pub struct Clock {
    pub player: Option<Player>,
    playing: bool,
    /// (wall time at play start, master time at play start) for the fallback.
    anchor: Option<(Instant, f64)>,
    paused_at: f64,
}

impl Clock {
    pub fn new() -> Self {
        let player = match Player::new() {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("audio output unavailable, using wall clock: {e:#}");
                None
            }
        };
        Self {
            player,
            playing: false,
            anchor: None,
            paused_at: 0.0,
        }
    }

    fn use_player(&self) -> bool {
        self.player.as_ref().map(|p| p.has_audio()).unwrap_or(false)
    }

    pub fn time(&self) -> f64 {
        if self.use_player() {
            return self.player.as_ref().unwrap().position();
        }
        match (self.playing, self.anchor) {
            (true, Some((start, t0))) => t0 + start.elapsed().as_secs_f64(),
            _ => self.paused_at,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        let t = self.time();
        self.playing = true;
        self.anchor = Some((Instant::now(), t));
        if let Some(p) = &self.player {
            p.play();
        }
    }

    pub fn pause(&mut self) {
        self.paused_at = self.time();
        self.playing = false;
        self.anchor = None;
        if let Some(p) = &self.player {
            p.pause();
        }
    }

    pub fn seek(&mut self, t: f64) {
        let t = t.max(0.0);
        self.paused_at = t;
        if self.playing {
            self.anchor = Some((Instant::now(), t));
        }
        if let Some(p) = &self.player {
            p.seek(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Jump far across a real project and require every camera to deliver
    /// the frame for the new time. Decoding 4K from a keyframe takes longer
    /// than the restart debounce, which once made the engine kill the
    /// decoder forever. Run with `TRIO_PROJECT=path cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn far_seek_delivers_frames() {
        let Ok(path) = std::env::var("TRIO_PROJECT") else {
            return;
        };
        let project = trio_core::project::load(Path::new(&path)).unwrap();
        let mut set = StreamSet::new(
            Quality::Preview { max_edge: 1280 },
            HwAccel::None,
            project.output.fps,
        );
        let half = 0.5 / set.fps;
        let jumps = [1400.0, 1450.0, 2700.0, 1500.0, 2800.0, 1420.0];
        for &t in &jumps {
            let started = Instant::now();
            loop {
                set.advance(&project, t, Mode::Live);
                let done = (0..CAMERA_COUNT).all(|cam| match project.clip_at(cam, t) {
                    Some((_, c)) => set
                        .current(cam)
                        .map(|f| (f.time - (t - c.offset)).abs() <= half + 1e-6)
                        .unwrap_or(false),
                    None => true,
                });
                if done {
                    eprintln!(
                        "t={t}: frames after {:?}, {} decoder starts so far",
                        started.elapsed(),
                        set.restarts
                    );
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "no frame for t={t} within 20 s"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Every jump may start each decoder once; anything more is a restart loop.
        assert!(
            set.restarts <= (jumps.len() * CAMERA_COUNT) as u64,
            "{} decoder starts for {} jumps",
            set.restarts,
            jumps.len()
        );
    }
}
