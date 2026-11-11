//! What a session's video is: its codecs, the viewer's limits and preset,
//! and the units a backend hands thread C.
use crate::{Result, error};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    Jpeg,
    H264,
    /// H.265 Main, 4:2:0. Never in a viewer's `codecs`: an older host
    /// rejects a value it does not know. A viewer that can decode it says so
    /// with the `h265` capability, and only a host that can encode it answers
    /// with it.
    H265,
}

/// What a viewer asks the host to favour. Each preset maps to bitrate and
/// fps targets on the host; changing it mid-session never reconnects. A
/// viewer listing `fps_override` makes its `limits.max_fps` every preset's
/// ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreset {
    Quality,
    #[default]
    Balanced,
    Smooth,
}

/// The largest picture a session encodes: 3840x2160.
pub(crate) const MAX_VIDEO_WIDTH: u32 = 3840;
pub(crate) const MAX_VIDEO_HEIGHT: u32 = 2160;
/// The smallest side of a viewer's limits.
pub(crate) const MIN_VIDEO_SIDE: u32 = 2;
/// The frame rates a viewer's limits may ask for.
pub(crate) const MIN_VIEWER_FPS: u32 = 30;
/// The highest frame rate anything paces at.
pub(crate) const MAX_FPS: u32 = 120;

/// Renderer limits, fixed for the session. Encoded dimensions preserve the
/// display's aspect ratio and never upscale; `Display` remains in physical
/// desktop pixels for input and pointer metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoLimits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}
impl VideoLimits {
    /// Reject invalid limits before opening a session or creating GPU resources.
    pub fn validate(&self) -> Result<()> {
        if !(MIN_VIDEO_SIDE..=MAX_VIDEO_WIDTH).contains(&self.max_width)
            || !(MIN_VIDEO_SIDE..=MAX_VIDEO_HEIGHT).contains(&self.max_height)
            || !(MIN_VIEWER_FPS..=MAX_FPS).contains(&self.max_fps)
        {
            return Err(error("video limits outside supported bounds"));
        }
        Ok(())
    }
    /// A viewer's limits by default: the display's own size, up to
    /// 3840x2160, at the display's refresh rate.
    pub fn for_viewer(refresh_hz: f64) -> Self {
        Self::choose(refresh_hz, Resolution::default(), None)
    }
    /// The one place a viewer's limits are derived: `resolution`'s bounds, at
    /// `max_fps` when its user chose one (30..=120), else at the display's
    /// refresh rate, at most 120 fps. Android then asks for 60: the mode a
    /// phone reports at connect may be a power-saving 30 or 48 Hz, and its
    /// presenter asks for the highest mode while the viewer shows. A refresh
    /// rate that is not finite and positive counts as 60 Hz.
    pub fn choose(refresh_hz: f64, resolution: Resolution, max_fps: Option<u32>) -> Self {
        let fps = if let Some(fps) = max_fps {
            fps.clamp(MIN_VIEWER_FPS, MAX_FPS)
        } else if cfg!(target_os = "android") {
            60
        } else if refresh_hz.is_finite() && refresh_hz > 0.0 {
            refresh_hz
                .round()
                .clamp(f64::from(MIN_VIEWER_FPS), f64::from(MAX_FPS)) as u32
        } else {
            60
        };
        let (max_width, max_height) = resolution.bounds();
        Self {
            max_width,
            max_height,
            max_fps: fps,
        }
    }
    /// These bounds fitted inside a decoder's largest picture, in either
    /// orientation: the long edge within the longer of `max`'s sides, the
    /// short edge within the shorter. Never below 2x2.
    pub fn clamped(self, max: (u32, u32)) -> Self {
        let (long, short) = (max.0.max(max.1), max.0.min(max.1));
        let (max_width, max_height) = if self.max_width >= self.max_height {
            (self.max_width.min(long), self.max_height.min(short))
        } else {
            (self.max_width.min(short), self.max_height.min(long))
        };
        Self {
            max_width: max_width.max(MIN_VIDEO_SIDE),
            max_height: max_height.max(MIN_VIDEO_SIDE),
            ..self
        }
    }
}

/// The largest picture a viewer asks for. The host keeps the display's
/// aspect ratio inside these bounds and never upscales, so a smaller display
/// is sent at its own size whatever is chosen. The bounds are landscape: a
/// portrait display is fitted into them as well, and a display larger than
/// 3840x2160 into 4K.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "1440p")]
    P1440,
    /// The display's own size, up to 3840x2160: the default.
    #[default]
    #[serde(rename = "native")]
    Native,
}

impl Resolution {
    /// Its bounds, width by height.
    pub const fn bounds(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
            Self::Native => (MAX_VIDEO_WIDTH, MAX_VIDEO_HEIGHT),
        }
    }
}
const _: () = {
    let presets = [
        Resolution::P720,
        Resolution::P1080,
        Resolution::P1440,
        Resolution::Native,
    ];
    let mut at = 0;
    while at < presets.len() {
        let (width, height) = presets[at].bounds();
        assert!(
            MIN_VIDEO_SIDE <= width
                && width <= MAX_VIDEO_WIDTH
                && MIN_VIDEO_SIDE <= height
                && height <= MAX_VIDEO_HEIGHT,
            "every preset's bounds pass VideoLimits::validate"
        );
        at += 1;
    }
};

/// Why a viewer asks for an independently decodable picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeReason {
    DecoderError,
    Gap,
    Backlog,
    Stall,
    User,
}

/// The encoder rate for one capture call: the QoS bitrate and the pace the
/// worker captures at. Neither change ever rebuilds the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoRate {
    pub bitrate: u32,
    pub pace_fps: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VideoFormat {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub pts_us: u64,
    #[serde(default)]
    pub duplicate: bool,
    /// Encoder submit to output, for the host's `host_us`. Zero when unknown.
    #[serde(default)]
    pub encode_us: u32,
    /// The GOP length the encoder read back when it was built, for the host's
    /// `gop`. Zero when unknown (JPEG, or an encoder that reports none). It
    /// rides with each unit, so it crosses the helper IPC as the unit does.
    #[serde(default)]
    pub gop: u32,
}

pub struct EncodedFrame {
    pub format: VideoFormat,
    pub bytes: Vec<u8>,
}
