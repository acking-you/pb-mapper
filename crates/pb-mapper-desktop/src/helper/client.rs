use super::*;
use tokio::net::windows::named_pipe::NamedPipeClient;

struct RemoteBackend {
    runtime: tokio::runtime::Runtime,
    pipe: Option<NamedPipeClient>,
    context: Option<DesktopContext>,
    video: Option<crate::VideoFormat>,
    cursor: Option<crate::CursorPosition>,
    sequence: u64,
    failed: bool,
}

/// Connect from the dedicated SDK capture worker. No automatic privilege
/// escalation or fallback: failure is observable at the viewer and local UI.
pub fn backend(display_id: u32) -> Result<Box<dyn Backend>> {
    status().ensure_ready()?;
    let config = configuration()?;
    connect_backend(display_id, &config)
}

/// The background supervisor uses the same bounded IPC as the UI. The server
/// independently accepts only its own kernel-reported PID, never arbitrary
/// SYSTEM callers, and binds the worker to the approved owner's current login.
#[cfg(feature = "windows-helper")]
pub(super) fn service_backend(display_id: u32) -> Result<Box<dyn Backend>> {
    let config = configuration()?;
    if os::current_sid()? != "S-1-5-18"
        || os::session(std::process::id())? != 0
        || !os::same_path(&std::env::current_exe()?, &config.helper_executable)
    {
        return Err(error("background capture requires the installed service"));
    }
    connect_backend(display_id, &config)
}

fn connect_backend(display_id: u32, config: &Configuration) -> Result<Box<dyn Backend>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let pipe = {
        let _entered = runtime.enter();
        let pipe = ipc::connect(PIPE)?;
        os::verify_server(&pipe, &config.helper_executable, None)?;
        pipe
    };
    let mut backend = RemoteBackend {
        runtime,
        pipe: Some(pipe),
        context: None,
        video: None,
        cursor: None,
        sequence: 0,
        failed: false,
    };
    backend.call(Request::Start { display_id })?;
    Ok(Box::new(backend))
}
impl RemoteBackend {
    fn call(&mut self, request: Request) -> Result<Vec<u8>> {
        if self.failed {
            return Err(error("helper disconnected; restart desktop sharing"));
        }
        let result = self.runtime.block_on(async {
            tokio::time::timeout(TIMEOUT, async {
                let pipe = self
                    .pipe
                    .as_mut()
                    .ok_or_else(|| error("helper disconnected"))?;
                crate::wire::write_json(pipe, &request).await?;
                let response: Response = crate::wire::read_json(pipe).await?;
                let image = crate::wire::read_bytes(pipe, crate::wire::MAX_FRAME).await?;
                if let Some(reason) = response.error {
                    self.context = None;
                    self.video = None;
                    self.cursor = None;
                    return Ok(Err(error(reason)));
                }
                self.context = response.context;
                self.video = response.video;
                self.cursor = response.cursor;
                Ok::<_, std::io::Error>(Ok(image))
            })
            .await
        });
        match result {
            Ok(Ok(result)) => result,
            failure => {
                self.failed = true;
                self.cursor = None;
                self.pipe.take();
                Err(error(match failure {
                    Ok(Err(e)) => e.to_string(),
                    Err(e) => e.to_string(),
                    _ => unreachable!(),
                }))
            }
        }
    }
}
impl Backend for RemoteBackend {
    fn configure_video(&mut self, limits: crate::VideoLimits, _nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        self.call(Request::ConfigureVideo {
            preferences: limits.into(),
        })?;
        Ok(())
    }
    fn display(&mut self) -> Result<crate::Display> {
        Ok(self.context()?.display)
    }
    fn context(&mut self) -> Result<DesktopContext> {
        self.call(Request::Context)?;
        self.context
            .clone()
            .ok_or_else(|| error("helper did not report desktop identity"))
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        self.call(Request::Capture)
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        if self.failed || self.context.as_ref() != Some(expected) {
            return Err(error("helper cursor belongs to a different desktop"));
        }
        Ok(self.cursor)
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: crate::VideoCodec,
        keyframe: bool,
        rate: crate::VideoRate,
    ) -> Result<Option<crate::EncodedFrame>> {
        let bytes = self.call(Request::CaptureVideo {
            expected: expected.clone(),
            codec,
            keyframe,
            bitrate: rate.bitrate,
        })?;
        Ok(self
            .video
            .take()
            .map(|format| crate::EncodedFrame { format, bytes }))
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| error("no current helper frame"))?
            .clone();
        self.input_in_context(&context, input)
    }
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        _wait: std::time::Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        let bytes = self.call(Request::DrainVideo {
            expected: expected.clone(),
        })?;
        Ok(self
            .video
            .take()
            .map(|format| crate::EncodedFrame { format, bytes }))
    }
    fn input_in_context(&mut self, expected: &DesktopContext, input: &Input) -> Result<()> {
        input.validate(&expected.display)?;
        self.sequence += 1;
        self.call(Request::Input {
            generation: expected.generation,
            sequence: self.sequence,
            input: input.clone(),
        })?;
        Ok(())
    }
    fn presented(&mut self, expected: &DesktopContext) -> Result<()> {
        self.call(Request::Presented {
            generation: expected.generation,
        })?;
        Ok(())
    }
    fn release(&mut self) {
        let _ = self.call(Request::Release);
    }
}
impl Drop for RemoteBackend {
    fn drop(&mut self) {
        if !self.failed {
            let _ = self.call(Request::Close);
        }
    }
}
