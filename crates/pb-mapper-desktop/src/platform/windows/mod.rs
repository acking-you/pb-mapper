//! The Windows host: the ordinary desktop (or, in the privileged helper's
//! worker, the bound input desktop), the fused DXGI to D3D11 to Media
//! Foundation encoder, enigo injection and the pointer, xcap stills; DPAPI;
//! the waitable timer and fine timers; and the privileged helper.
//! Uses published Apache-2.0 xcap and MIT enigo. No RustDesk source is copied.

pub(crate) mod cursor;
pub(crate) mod desktop;
pub mod dpapi;
pub(crate) mod helper;
pub(crate) mod input;
pub(crate) mod mf;
pub(crate) mod modes;
pub(crate) mod still;
pub(crate) mod timer;
pub mod video;
pub(crate) mod waiter;
pub(crate) mod win32;

use super::{HostSupport, Parts, Platform, UnsupportedReason, VideoSource};
use crate::{
    DRAIN_POLL, Display, DisplaySelection, EncodedFrame, HostOs, Result, VideoCodec, VideoLimits,
    VideoRate, Waker,
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) enum Windows {}

impl Platform for Windows {
    const OS: HostOs = HostOs::Windows;
    type Grant = ();
    type Desktop = desktop::WinDesktop;
    type Video = video::Encoder;
    type Input = input::WinInput;
    type Cursor = cursor::WinCursor;
    type Modes = modes::WinModes;
    type Still = still::XcapJpeg;

    /// A Windows host encodes H.264 with Media Foundation, and H.265 where
    /// its GPU has an encoder for it.
    fn support() -> HostSupport {
        let host = mf::media_foundation();
        HostSupport {
            host,
            viewer: true,
            secure_desktop: false,
            os: Some(HostOs::Windows),
            reason: (!host).then_some(UnsupportedReason::MediaFoundationMissing),
            permissions: None,
        }
    }
    fn displays() -> Result<Vec<Display>> {
        desktop::displays()
    }
    fn acquire(selection: &DisplaySelection) -> impl Future<Output = Result<((), Display)>> + Send {
        std::future::ready(desktop::resolve(selection).map(|display| ((), display)))
    }
    fn open(_: &(), display: &Display) -> Result<Parts<Self>> {
        desktop::available()?;
        parts(desktop::WinDesktop::open(display)?)
    }
    fn default_state_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(|local| PathBuf::from(local).join("pb-mapper").join("desktop"))
    }
    fn recover(state_dir: &Path) -> Result<usize> {
        crate::modes::recover(
            &mut modes::WinModes,
            &crate::modes::Journal::in_dir(Some(state_dir.to_path_buf())),
        )
    }
}

/// The privileged worker's backend: bound to the input desktop of its
/// approved console login, the secure desktop included.
#[cfg(feature = "windows-helper")]
pub(crate) fn open_bound(
    display_id: u32,
    display_device: Option<String>,
    session: crate::oop::proto::SessionIdentity,
    journal: crate::modes::Journal,
) -> Result<crate::native::PlatformBackend<Windows>> {
    let mut binding = helper::binding::DesktopBinding::new(session)?;
    binding.enter(|| {})?;
    let desktop = desktop::WinDesktop::bound(display_id, display_device, binding)?;
    Ok(crate::native::PlatformBackend::from_parts(
        std::sync::Arc::new(()),
        parts(desktop)?,
        journal,
    ))
}

/// A Windows host's parts on `desktop`, which is built first.
fn parts(desktop: desktop::WinDesktop) -> Result<Parts<Windows>> {
    Ok(Parts {
        input: input::WinInput::new()?,
        cursor: cursor::WinCursor::default(),
        modes: modes::WinModes,
        still: still::XcapJpeg,
        desktop,
    })
}

/// The fused encoder, unchanged inside: each submit captures and encodes.
impl VideoSource for video::Encoder {
    type Grant = ();
    const DRAIN: Option<Duration> = Some(DRAIN_POLL);
    fn encodes(display: &Display, codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::H265 => video::h265_available(display.id),
            VideoCodec::H264 => true,
            VideoCodec::Jpeg => false,
        }
    }
    fn open(
        _: &(),
        display: &Display,
        codec: VideoCodec,
        limits: VideoLimits,
        rate: VideoRate,
        _: &Waker,
    ) -> Result<Self> {
        Self::new_with_size(
            display,
            codec,
            limits.max_fps,
            rate.bitrate,
            limits.max_width,
            limits.max_height,
        )
    }
    fn codec(&self) -> VideoCodec {
        video::Encoder::codec(self)
    }
    fn set_rate(&mut self, rate: VideoRate) -> Result<()> {
        video::Encoder::set_rate(self, rate)
    }
    fn submit(&mut self, keyframe: bool) -> Result<()> {
        video::Encoder::submit(self, keyframe).map(|_| ())
    }
    fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        video::Encoder::drain(self).map(|image| image.map(Into::into))
    }
    fn pending(&self) -> usize {
        self.pending_len()
    }
}
