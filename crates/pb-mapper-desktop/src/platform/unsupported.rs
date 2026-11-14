//! Every operating system that does not host yet. Its parts are uninhabited,
//! so it compiles to nothing; it refuses before a host registers.

use super::{
    CursorSource, Desktop, HostSupport, Injector, NoModes, Parts, Platform, StillCapture,
    UnsupportedReason, VideoSource,
};
use crate::{
    BackendError, Button, CursorMode, Display, DisplaySelection, EncodedFrame, FailureKind, HostOs,
    Key, Result, VideoCodec, VideoLimits, VideoRate, Waker,
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) enum Unsupported {}

/// A part no unsupported platform ever builds.
pub(crate) enum Never {}

fn unsupported() -> std::io::Error {
    BackendError::new(
        FailureKind::Unsupported,
        "Desktop hosting requires Windows, or macOS 12.3 or later",
    )
    .into()
}

impl Platform for Unsupported {
    /// This build's operating system, though it does not host.
    const OS: HostOs = if cfg!(target_os = "android") {
        HostOs::Android
    } else {
        HostOs::Linux
    };
    type Grant = Never;
    type Desktop = Never;
    type Video = Never;
    type Input = Never;
    type Cursor = Never;
    type Modes = NoModes;
    type Still = Never;

    fn support() -> HostSupport {
        HostSupport {
            host: false,
            viewer: true,
            secure_desktop: false,
            os: None,
            reason: Some(UnsupportedReason::Platform),
            permissions: None,
        }
    }
    fn displays() -> Result<Vec<Display>> {
        Ok(Vec::new())
    }
    fn acquire(_: &DisplaySelection) -> impl Future<Output = Result<(Never, Display)>> + Send {
        std::future::ready(Err(unsupported()))
    }
    fn open(grant: &Never, _: &Display) -> Result<Parts<Self>> {
        match *grant {}
    }
    fn default_state_dir() -> Option<PathBuf> {
        None
    }
    fn recover(_: &Path) -> Result<usize> {
        Ok(0)
    }
}

impl Desktop for Never {
    type Scope = Never;
    fn scope(&self) -> Never {
        match *self {}
    }
    fn prepare(&mut self, _: impl FnOnce()) -> Result<()> {
        match *self {}
    }
    fn display(&mut self) -> Result<Display> {
        match *self {}
    }
}

impl VideoSource for Never {
    type Grant = Never;
    const DRAIN: Option<Duration> = None;
    fn encodes(_: &Display, _: VideoCodec) -> bool {
        false
    }
    fn open(
        grant: &Never,
        _: &Display,
        _: VideoCodec,
        _: VideoLimits,
        _: VideoRate,
        _: &Waker,
    ) -> Result<Self> {
        match *grant {}
    }
    fn codec(&self) -> VideoCodec {
        match *self {}
    }
    fn set_rate(&mut self, _: VideoRate) -> Result<()> {
        match *self {}
    }
    fn submit(&mut self, _: bool) -> Result<()> {
        match *self {}
    }
    fn drain(&mut self) -> Result<Option<EncodedFrame>> {
        match *self {}
    }
    fn pending(&self) -> usize {
        match *self {}
    }
}

impl Injector for Never {
    const LOCK: bool = false;
    fn pointer(&mut self, _: i32, _: i32) -> Result<()> {
        match *self {}
    }
    fn button(&mut self, _: Button, _: bool) -> Result<()> {
        match *self {}
    }
    fn wheel(&mut self, _: i32, _: i32) -> Result<()> {
        match *self {}
    }
    fn key(&mut self, _: Key, _: bool) -> Result<()> {
        match *self {}
    }
    fn text(&mut self, _: &str) -> Result<()> {
        match *self {}
    }
    fn lock(&mut self) -> Result<()> {
        match *self {}
    }
}

impl CursorSource for Never {
    const MODE: CursorMode = CursorMode::None;
    fn read(&mut self) -> Result<Option<(i32, i32, bool)>> {
        match *self {}
    }
}

impl StillCapture for Never {
    fn jpeg(&mut self, _: &Display) -> Result<Vec<u8>> {
        match *self {}
    }
}
