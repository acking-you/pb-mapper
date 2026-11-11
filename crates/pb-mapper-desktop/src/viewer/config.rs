//! How a viewer connects: the codecs, limits and preset it offers, and
//! the stages it reports on the way.
use super::ConnectError;
use crate::{
    QualityPreset, VideoCodec, VideoLimits,
    protocol::{
        CAPABILITY_CURSOR_SEQUENCE, CAPABILITY_DISPLAY_MODE, CAPABILITY_ENCODER_DWELL,
        CAPABILITY_FPS_OVERRIDE, CAPABILITY_H265,
    },
};
use serde::Serialize;
use std::sync::Arc;

/// How a viewer connects.
#[derive(Clone)]
pub struct ViewerConfig {
    /// The codecs the renderer decodes, in preference order: one or two of
    /// H.264 and JPEG, and `H265` beside them when it decodes that too. H.265
    /// never goes in `Authenticate.codecs`, which an older host rejects: it is
    /// offered as the `h265` capability, and only a host that can encode it
    /// answers with it. Whatever the host picks, it is one of these.
    pub codecs: Vec<VideoCodec>,
    /// Fixed for the session; see `VideoLimits::choose`.
    pub limits: VideoLimits,
    /// `limits.max_fps` is a frame rate the user chose, sent as the
    /// `fps_override` capability: the host's ceiling for every preset, and
    /// the fps cap's, in place of each preset's own.
    pub fps_override: bool,
    /// The initial preset; `Viewer::set_quality` changes it live.
    pub quality: QualityPreset,
    /// Called as the connection passes each stage.
    pub progress: Option<Arc<dyn Fn(ConnectStage) + Send + Sync>>,
}
/// What a renderer decodes in hardware: the largest picture each decoder
/// takes at 60 fps, in either orientation; `None` where it has none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decoders {
    pub h264: Option<(u32, u32)>,
    pub h265: Option<(u32, u32)>,
}

impl ViewerConfig {
    /// A viewer of `codecs`, at the default resolution and the display's 60
    /// fps, with the balanced preset.
    pub fn new(codecs: Vec<VideoCodec>) -> Self {
        Self {
            codecs,
            limits: VideoLimits::choose(60.0, crate::Resolution::default(), None),
            fps_override: false,
            quality: QualityPreset::Balanced,
            progress: None,
        }
    }
    /// The offer for what the renderer decodes: H.265 where it decodes it,
    /// then H.264 where it decodes that, else JPEG, which needs no decoder.
    /// The limits are `resolution`'s at `max_fps`, or at the display's
    /// `refresh_hz` (see [`VideoLimits::choose`]), fitted inside the largest
    /// picture of the decoder the host picks first: so a phone keeps H.265
    /// at the default size. A host that declines H.265 sends H.264 within
    /// those limits, which the H.264 decoder may not take; connect again
    /// without H.265 then.
    pub fn for_decoders(
        decoders: Decoders,
        resolution: crate::Resolution,
        refresh_hz: f64,
        max_fps: Option<u32>,
    ) -> Self {
        let fallback = if decoders.h264.is_some() {
            VideoCodec::H264
        } else {
            VideoCodec::Jpeg
        };
        let codecs = decoders
            .h265
            .map(|_| VideoCodec::H265)
            .into_iter()
            .chain([fallback])
            .collect();
        let limits = VideoLimits::choose(refresh_hz, resolution, max_fps);
        Self {
            codecs,
            limits: match decoders.h265.or(decoders.h264) {
                Some(largest) => limits.clamped(largest),
                None => limits,
            },
            fps_override: max_fps.is_some(),
            quality: QualityPreset::Balanced,
            progress: None,
        }
    }
    /// H.265 at most once, and beside one or two codecs every host knows.
    pub(super) fn validate(&self) -> std::result::Result<(), ConnectError> {
        let offered = self.wire_codecs().len();
        let h265 = self
            .codecs
            .iter()
            .filter(|codec| **codec == VideoCodec::H265)
            .count();
        if !(1..=2).contains(&offered) || h265 > 1 || self.limits.validate().is_err() {
            return Err(ConnectError::InvalidRequest);
        }
        Ok(())
    }
    /// What `Authenticate.codecs` lists: every codec but H.265.
    pub(super) fn wire_codecs(&self) -> Vec<VideoCodec> {
        self.codecs
            .iter()
            .copied()
            .filter(|codec| *codec != VideoCodec::H265)
            .collect()
    }
    /// What this viewer reads and asks for, as `Authenticate.capabilities`.
    pub(super) fn capabilities(&self) -> Vec<String> {
        let mut capabilities = vec![
            CAPABILITY_ENCODER_DWELL.to_owned(),
            CAPABILITY_CURSOR_SEQUENCE.to_owned(),
            CAPABILITY_DISPLAY_MODE.to_owned(),
        ];
        if self.codecs.contains(&VideoCodec::H265) {
            capabilities.push(CAPABILITY_H265.to_owned());
        }
        if self.fps_override {
            capabilities.push(CAPABILITY_FPS_OVERRIDE.to_owned());
        }
        capabilities
    }
    pub(super) fn progress(&self, stage: ConnectStage) {
        if let Some(progress) = &self.progress {
            progress(stage);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectStage {
    /// Opening the tunnel to the host, direct or through the relay.
    Path,
    /// TLS, pinned to the invitation's certificate fingerprint.
    Identity,
    /// The invitation's token, answered by `Accepted` or `Rejected`.
    Authorizing,
}
