use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CAMERA_COUNT: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub wav: Option<PathBuf>,
    pub cameras: Vec<Camera>,
    pub layout: LayoutId,
    pub slots: [Slot; CAMERA_COUNT],
    pub output: OutputSettings,
    pub range: Range,
    /// Slow automatic pan and zoom on top of the slot framing.
    #[serde(default)]
    pub motion: Motion,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: 1,
            wav: None,
            cameras: (0..CAMERA_COUNT)
                .map(|i| Camera {
                    name: format!("Cam {}", i + 1),
                    ..Default::default()
                })
                .collect(),
            layout: LayoutId::HBigLeft,
            slots: [
                Slot {
                    camera: 0,
                    ..Default::default()
                },
                Slot {
                    camera: 1,
                    ..Default::default()
                },
                Slot {
                    camera: 2,
                    ..Default::default()
                },
            ],
            output: OutputSettings::default(),
            range: Range {
                start: 0.0,
                end: 0.0,
            },
            motion: Motion::Subtle,
        }
    }
}

impl Project {
    /// Master timeline length: the WAV if present, else the last clip end.
    pub fn duration(&self, wav_duration: Option<f64>) -> f64 {
        let clips_end = self
            .cameras
            .iter()
            .flat_map(|c| c.clips.iter())
            .map(|c| c.end())
            .fold(0.0_f64, f64::max);
        match wav_duration {
            Some(w) if w > 0.0 => w,
            _ => clips_end,
        }
    }

    /// The clip of `camera` showing at master time `t`, if any.
    pub fn clip_at(&self, camera: usize, t: f64) -> Option<(usize, &Clip)> {
        self.clip_time(camera, t).map(|(i, c, _)| (i, c))
    }

    /// The clip of `camera` showing at master time `t` and the seconds into
    /// that clip to show, honouring the camera's picture delay and the
    /// clip's clock speed.
    pub fn clip_time(&self, camera: usize, t: f64) -> Option<(usize, &Clip, f64)> {
        self.cameras.get(camera)?.clip_at(t)
    }

    /// Effective output size honoring the layout orientation.
    pub fn output_size(&self) -> (u32, u32) {
        let (w, h) = (self.output.width, self.output.height);
        match self.layout.orientation() {
            Orientation::Horizontal => (w.max(h), w.min(h)),
            Orientation::Vertical => (w.min(h), w.max(h)),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Camera {
    pub name: String,
    pub folder: Option<PathBuf>,
    #[serde(default)]
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub grade: Grade,
    /// Where the people are in this camera's picture, found automatically.
    #[serde(default)]
    pub subject: Option<Subject>,
    /// Seconds the picture is shown later than the audio sync put it.
    /// Sound reaches a camera about 3 ms per metre after the picture, and
    /// the sync lines up what the camera heard, so a camera far from the
    /// band shows its picture early by that much; this takes it back.
    #[serde(default)]
    pub picture_delay: f64,
}

impl Camera {
    /// The clip showing at master time `t` and the seconds into it to
    /// show, honouring the picture delay and each clip's clock speed.
    pub fn clip_at(&self, t: f64) -> Option<(usize, &Clip, f64)> {
        let t = t - self.picture_delay;
        self.clips
            .iter()
            .enumerate()
            .find(|(_, c)| c.covers(t))
            .map(|(i, c)| (i, c, c.local_time(t)))
    }
}

/// Where the people are in a camera's picture over time, one sample per
/// moment analysed. See `autoframe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Subject {
    pub samples: Vec<SubjectSample>,
}

/// Boxes are x0, y0, x1, y1 in 0..1 of the (rotated) source frame.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SubjectSample {
    /// Master time of the sample.
    pub t: f64,
    /// Where the faces are; stays in view whatever the movement.
    pub faces: [f32; 4],
    /// Everything that moves: the whole band and their instruments.
    pub people: [f32; 4],
}

/// How much the automatic movement pans and zooms.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Off,
    Subtle,
    Lively,
}

impl Motion {
    pub const ALL: [Motion; 3] = [Motion::Off, Motion::Subtle, Motion::Lively];

    /// 0 = still, 1 = the full movement.
    pub fn amount(self) -> f32 {
        match self {
            Motion::Off => 0.0,
            Motion::Subtle => 0.5,
            Motion::Lively => 1.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Motion::Off => "Off",
            Motion::Subtle => "Subtle",
            Motion::Lively => "Lively",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub path: PathBuf,
    /// Seconds.
    pub duration: f64,
    /// Display size after rotation is applied.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub rotation: i32,
    pub hdr: bool,
    pub has_audio: bool,
    pub creation_time: Option<String>,
    /// The container stamps the end of the recording rather than the start.
    /// Android's MediaRecorder writes `creation_time` when it finalises the
    /// file, so its clips carry the stop time.
    #[serde(default)]
    pub end_stamped: bool,
    /// Master-timeline seconds at which this clip's first frame appears.
    pub offset: f64,
    /// Master seconds per clip second, found by the audio sync: the camera's
    /// clock against the recorder's. 1.0 until the sync measured it; a
    /// phone typically lands within ±100 ppm (0.0001) of that.
    #[serde(default = "default_speed")]
    pub speed: f64,
    /// 0..1, present when auto-sync ran.
    pub sync_confidence: Option<f32>,
}

fn default_speed() -> f64 {
    1.0
}

impl Clip {
    /// Master time at which the clip ends.
    pub fn end(&self) -> f64 {
        self.offset + self.duration * self.speed
    }
    /// Seconds into the clip that play at master time `t`.
    pub fn local_time(&self, t: f64) -> f64 {
        (t - self.offset) / self.speed
    }
    pub fn covers(&self, t: f64) -> bool {
        t >= self.offset && t < self.end()
    }
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Slot {
    pub camera: usize,
    /// 1.0 = cover-fit, larger zooms in.
    pub zoom: f32,
    /// Normalized shift of the crop window, roughly -0.5..0.5.
    pub pan: [f32; 2],
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            camera: 0,
            zoom: 1.0,
            pan: [0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Grade {
    /// Stops.
    pub exposure: f32,
    /// 1.0 neutral.
    pub contrast: f32,
    /// 1.0 neutral.
    pub saturation: f32,
    /// -1..1, negative = cooler.
    pub temperature: f32,
    /// -1..1, negative = green, positive = magenta.
    pub tint: f32,
    pub lift: f32,
    pub gamma: f32,
    pub gain: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            temperature: 0.0,
            tint: 0.0,
            lift: 0.0,
            gamma: 1.0,
            gain: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LayoutId {
    HThree,
    HBigLeft,
    HBigCenterPip,
    VThree,
    VBigTop,
    VFullPip,
}

impl LayoutId {
    pub const ALL: [LayoutId; 6] = [
        LayoutId::HThree,
        LayoutId::HBigLeft,
        LayoutId::HBigCenterPip,
        LayoutId::VThree,
        LayoutId::VBigTop,
        LayoutId::VFullPip,
    ];

    pub fn orientation(self) -> Orientation {
        match self {
            LayoutId::HThree | LayoutId::HBigLeft | LayoutId::HBigCenterPip => {
                Orientation::Horizontal
            }
            LayoutId::VThree | LayoutId::VBigTop | LayoutId::VFullPip => Orientation::Vertical,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LayoutId::HThree => "Three side by side",
            LayoutId::HBigLeft => "Big left, two stacked right",
            LayoutId::HBigCenterPip => "Full frame, two small overlays",
            LayoutId::VThree => "Three stacked",
            LayoutId::VBigTop => "Big top, two below",
            LayoutId::VFullPip => "Full frame, two small overlays",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Codec {
    H264Software,
    H265Software,
    H264Vaapi,
    H265Vaapi,
    H264VideoToolbox,
    H265VideoToolbox,
}

impl Codec {
    pub const ALL: [Codec; 6] = [
        Codec::H264Software,
        Codec::H265Software,
        Codec::H264Vaapi,
        Codec::H265Vaapi,
        Codec::H264VideoToolbox,
        Codec::H265VideoToolbox,
    ];
    pub fn is_h265(self) -> bool {
        matches!(
            self,
            Codec::H265Software | Codec::H265Vaapi | Codec::H265VideoToolbox
        )
    }

    pub fn is_gpu(self) -> bool {
        !matches!(self, Codec::H264Software | Codec::H265Software)
    }

    /// The software encoder for this format.
    pub fn software(h265: bool) -> Codec {
        if h265 {
            Codec::H265Software
        } else {
            Codec::H264Software
        }
    }

    /// Same encoder engine, other format.
    pub fn with_h265(self, h265: bool) -> Codec {
        match (self, h265) {
            (Codec::H264Software | Codec::H265Software, false) => Codec::H264Software,
            (Codec::H264Software | Codec::H265Software, true) => Codec::H265Software,
            (Codec::H264Vaapi | Codec::H265Vaapi, false) => Codec::H264Vaapi,
            (Codec::H264Vaapi | Codec::H265Vaapi, true) => Codec::H265Vaapi,
            (Codec::H264VideoToolbox | Codec::H265VideoToolbox, false) => Codec::H264VideoToolbox,
            (Codec::H264VideoToolbox | Codec::H265VideoToolbox, true) => Codec::H265VideoToolbox,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Codec::H264Software => "H.264 (libx264, software)",
            Codec::H265Software => "H.265 (libx265, software)",
            Codec::H264Vaapi => "H.264 (VAAPI, Linux GPU)",
            Codec::H265Vaapi => "H.265 (VAAPI, Linux GPU)",
            Codec::H264VideoToolbox => "H.264 (VideoToolbox, macOS)",
            Codec::H265VideoToolbox => "H.265 (VideoToolbox, macOS)",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSettings {
    /// Long edge and short edge; orientation comes from the layout.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// The encoder to use when `auto_encoder` is off; otherwise only its
    /// format (H.264 or H.265) matters and the engine is picked at export.
    pub codec: Codec,
    /// Use the graphics card encoder whenever one was detected.
    #[serde(default = "default_true")]
    pub auto_encoder: bool,
    /// Quality knob: CRF for software encoders, QP for hardware ones.
    pub quality: u32,
    pub path: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            codec: Codec::H264Software,
            auto_encoder: true,
            quality: 18,
            path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Range {
    pub start: f64,
    pub end: f64,
}
