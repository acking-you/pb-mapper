use super::*;
use tokio::net::windows::named_pipe::NamedPipeClient;

struct RemoteBackend {
    runtime: tokio::runtime::Runtime,
    pipe: Option<NamedPipeClient>,
    context: Option<DesktopContext>,
    video: Option<crate::VideoFormat>,
    cursor: Option<crate::CursorPosition>,
    /// The worker encoder's pending units, from the last response.
    pending: usize,
    /// From the `Start` response: the interactive session's values, which
    /// differ from this process's when it is the SYSTEM service in session 0.
    double_click: Option<crate::DoubleClickGuard>,
    /// From the `H265` response.
    h265: Option<bool>,
    /// The last capture's codec and rate, repeated by drains.
    codec: crate::VideoCodec,
    rate: crate::VideoRate,
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
    let mut backend = RemoteBackend::new(runtime, pipe);
    backend.call(Request::Start { display_id })?;
    Ok(Box::new(backend))
}
impl RemoteBackend {
    fn new(runtime: tokio::runtime::Runtime, pipe: NamedPipeClient) -> Self {
        Self {
            runtime,
            pipe: Some(pipe),
            context: None,
            video: None,
            cursor: None,
            pending: 0,
            double_click: None,
            h265: None,
            codec: crate::VideoCodec::H264,
            rate: crate::VideoRate {
                bitrate: 0,
                pace_fps: 0,
            },
            sequence: 0,
            failed: false,
        }
    }

    /// One request and its response. A lost or stalled pipe fails every later
    /// call with `NotConnected`, which ends the session: a reinstalled or
    /// crashed helper never leaves a viewer waiting. The desktop and pointer
    /// the helper last reported stay until a response reports others: Release
    /// and Close report no desktop; Context, Capture and Video observe the
    /// pointer, and so does an applied input when the worker could read it,
    /// so the pointer is never older than the last input; and an error
    /// answers the request, not the desktop. The engine's own context checks
    /// notice a desktop change.
    fn call(&mut self, request: Request) -> Result<Vec<u8>> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        let reports = !matches!(request, Request::Release {} | Request::Close {});
        let observes = matches!(
            request,
            Request::Context {} | Request::Capture {} | Request::Video { .. }
        );
        let applies = matches!(request, Request::Input { .. });
        let result = self.runtime.block_on(async {
            tokio::time::timeout(TIMEOUT, async {
                let pipe = self
                    .pipe
                    .as_mut()
                    .ok_or_else(|| error("helper disconnected"))?;
                ipc::write_json(pipe, &request).await?;
                let response: Response = ipc::read_json(pipe).await?;
                let image = ipc::read_bytes(pipe, ipc::MAX_FRAME).await?;
                self.pending = response.pending as usize;
                if let Some(reason) = response.error {
                    self.video = None;
                    return Ok(Err(match legacy_kind(&reason) {
                        Some(kind) => crate::BackendError::new(kind, reason).into(),
                        None => error(reason),
                    }));
                }
                if reports {
                    self.context = response.context;
                }
                if observes || (applies && response.cursor.is_some()) {
                    self.cursor = response.cursor;
                }
                self.video = response.video;
                self.double_click = response.double_click.or(self.double_click);
                self.h265 = response.h265.or(self.h265);
                Ok::<_, std::io::Error>(Ok(image))
            })
            .await
        });
        match result {
            Ok(Ok(result)) => result,
            Ok(Err(failure)) => Err(self.fail(failure)),
            Err(elapsed) => Err(self.fail(elapsed)),
        }
    }

    fn fail(&mut self, failure: impl std::fmt::Display) -> std::io::Error {
        self.failed = true;
        self.cursor = None;
        self.pending = 0;
        self.pipe.take();
        disconnected(failure)
    }

    fn video(
        &mut self,
        expected: &DesktopContext,
        submit: bool,
        keyframe: bool,
        wait: std::time::Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        let bytes = self.call(Request::Video {
            expected: expected.clone(),
            codec: self.codec,
            submit,
            keyframe,
            rate: self.rate,
            wait_ms: u32::try_from(wait.as_millis())
                .unwrap_or(u32::MAX)
                .min(DRAIN_WAIT_MAX_MS),
        })?;
        Ok(self
            .video
            .take()
            .map(|format| crate::EncodedFrame { format, bytes }))
    }
}
/// The kind of a worker failure, which helper protocol 6 carries only as the
/// worker backend's text.
fn legacy_kind(text: &str) -> Option<crate::FailureKind> {
    use crate::FailureKind;
    if text.starts_with("secure_desktop_requires_helper") {
        Some(FailureKind::SecureDesktop)
    } else if text.starts_with(crate::backend::H265_ENCODER_FAILED) {
        Some(FailureKind::EncoderFailed {
            codec: crate::VideoCodec::H265,
        })
    } else if text.contains("display is unavailable")
        || text.contains("no longer available")
        || text.contains("output disappeared")
    {
        Some(FailureKind::DisplayLost)
    } else {
        None
    }
}
fn disconnected(failure: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotConnected, failure.to_string())
}
impl Backend for RemoteBackend {
    fn configure_video(&mut self, limits: crate::VideoLimits, nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        self.call(Request::ConfigureVideo {
            limits,
            nominal_fps,
        })?;
        Ok(())
    }
    fn display(&mut self) -> Result<crate::Display> {
        Ok(self.context()?.display)
    }
    fn context(&mut self) -> Result<DesktopContext> {
        self.call(Request::Context {})?;
        self.context
            .clone()
            .ok_or_else(|| error("helper did not report desktop identity"))
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        self.call(Request::Capture {})
    }
    fn cursor(&mut self, expected: &DesktopContext) -> Result<Option<crate::CursorPosition>> {
        if self.failed {
            return Err(disconnected("helper disconnected; restart desktop sharing"));
        }
        if self.context.as_ref() != Some(expected) {
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
        self.codec = codec;
        self.rate = rate;
        self.video(expected, true, keyframe, std::time::Duration::ZERO)
    }
    fn drain_video(
        &mut self,
        expected: &DesktopContext,
        wait: std::time::Duration,
    ) -> Result<Option<crate::EncodedFrame>> {
        self.video(expected, false, false, wait)
    }
    fn pending_video(&self) -> usize {
        self.pending
    }
    /// The worker probes the GPU of the display it captures, in the
    /// interactive session, which the background host in session 0 cannot;
    /// it is asked only for a viewer that decodes H.265.
    fn describe(&mut self, probe_h265: bool) -> Result<crate::Description> {
        let display = self.display()?;
        let h265 = probe_h265 && self.call(Request::H265 {}).is_ok() && self.h265 == Some(true);
        Ok(crate::Description {
            display,
            caps: crate::BackendCaps {
                h265,
                lock: true,
                os: Some(crate::HostOs::Windows),
                ..crate::BackendCaps::default()
            },
        })
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| error("no current helper frame"))?
            .clone();
        self.input_in_context(&context, input)
    }
    fn input_in_context(&mut self, expected: &DesktopContext, input: &Input) -> Result<()> {
        input.validate(&expected.display)?;
        self.sequence += 1;
        self.call(Request::Input {
            expected: expected.clone(),
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
        let _ = self.call(Request::Release {});
    }
    /// Never read here: the background host runs in the SYSTEM service in
    /// session 0, whose values are defaults, not the user's.
    fn double_click_guard(&mut self) -> Option<crate::DoubleClickGuard> {
        self.double_click
    }
}
impl Drop for RemoteBackend {
    fn drop(&mut self) {
        if !self.failed {
            let _ = self.call(Request::Close {});
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ServerOptions;

    /// A `RemoteBackend` on a private pipe whose helper answers each request
    /// with `answer` and an empty image, until `Close`.
    fn remote(
        answer: impl Fn(&Request) -> Response + Send + 'static,
    ) -> (RemoteBackend, std::thread::JoinHandle<()>) {
        let name = format!(
            r"\\.\pipe\pb-mapper.desktop-helper-test.{}",
            crate::security::secret().unwrap()
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut server = {
            let _entered = runtime.enter();
            ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .unwrap()
        };
        let helper = std::thread::spawn(move || {
            runtime.block_on(async move {
                server.connect().await.unwrap();
                loop {
                    let request: Request = ipc::read_json(&mut server).await.unwrap();
                    ipc::write_json(&mut server, &answer(&request))
                        .await
                        .unwrap();
                    ipc::write_bytes(&mut server, &[], ipc::MAX_FRAME)
                        .await
                        .unwrap();
                    if matches!(request, Request::Close {}) {
                        return;
                    }
                }
            });
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pipe = {
            let _entered = runtime.enter();
            ipc::connect(&name).unwrap()
        };
        (RemoteBackend::new(runtime, pipe), helper)
    }

    fn empty() -> Response {
        Response {
            context: None,
            error: None,
            video: None,
            cursor: None,
            pending: 0,
            double_click: None,
            h265: None,
        }
    }

    fn desktop() -> DesktopContext {
        DesktopContext {
            display: crate::Display {
                id: 1,
                name: "helper".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            generation: 1,
            desktop: crate::DesktopKind::Ordinary,
        }
    }

    /// The desktop kind crosses the pipe as protocol 6 wrote its string, and
    /// a protocol 6 worker's failure texts read back as their kinds.
    #[test]
    fn protocol_6_desktops_and_failures_read_as_kinds() {
        let secure = DesktopContext {
            desktop: crate::DesktopKind::Secure,
            ..desktop()
        };
        assert_eq!(
            serde_json::to_string(&secure).unwrap(),
            r#"{"display":{"id":1,"name":"helper","x":0,"y":0,"width":64,"height":64},"generation":1,"desktop":"secure"}"#
        );
        let ordinary: DesktopContext = serde_json::from_str(
            r#"{"display":{"id":1,"name":"helper","x":0,"y":0,"width":64,"height":64},"generation":1,"desktop":"ordinary"}"#,
        )
        .unwrap();
        assert_eq!(ordinary, desktop());
        use crate::FailureKind::{DisplayLost, EncoderFailed, SecureDesktop};
        for (text, kind) in [
            ("secure_desktop_requires_helper: UAC", Some(SecureDesktop)),
            ("display is unavailable", Some(DisplayLost)),
            ("selected display is no longer available", Some(DisplayLost)),
            ("selected GPU output disappeared", Some(DisplayLost)),
            (
                "h265_encoder_failed: hardware video encoder stalled",
                Some(EncoderFailed {
                    codec: crate::VideoCodec::H265,
                }),
            ),
            ("desktop capture delivered no image", None),
        ] {
            assert_eq!(legacy_kind(text), kind, "{text}");
        }
    }

    #[test]
    fn a_helper_error_keeps_the_desktop_and_its_pointer() {
        let desktop = desktop();
        let pointer = crate::CursorPosition {
            x: 3,
            y: 4,
            visible: true,
        };
        // A helper whose LockWorkStation fails: Input gets an error, Release
        // and Close report no desktop, Presented reports it without looking
        // at the pointer, and the rest report both.
        let reported = desktop.clone();
        let (mut backend, helper) = remote(move |request| {
            let mut response = empty();
            match request {
                Request::Input { .. } => response.error = Some("lock failed".into()),
                Request::Release {} | Request::Close {} => {}
                Request::Presented { .. } => response.context = Some(reported.clone()),
                _ => {
                    response.context = Some(reported.clone());
                    response.cursor = Some(pointer);
                }
            }
            response
        });
        assert_eq!(backend.context().unwrap(), desktop);
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        // Neither a failed lock nor a release says the desktop changed, and
        // a lease renewal does not observe the pointer.
        assert!(backend.input_in_context(&desktop, &Input::Lock {}).is_err());
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        backend.release();
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        backend.presented(&desktop).unwrap();
        assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
        drop(backend);
        helper.join().unwrap();
    }

    #[test]
    fn a_helper_hosted_unit_carries_the_encoder_gop() {
        // The worker's encoder read back an endless GOP; thread C reports the
        // GOP of the units it takes from this backend.
        let (mut backend, helper) = remote(|request| Response {
            video: matches!(request, Request::Video { .. }).then(|| crate::VideoFormat {
                codec: crate::VideoCodec::H264,
                width: 64,
                height: 64,
                keyframe: true,
                gop: u32::MAX,
                ..crate::VideoFormat::default()
            }),
            ..empty()
        });
        let rate = crate::VideoRate {
            bitrate: 4_000_000,
            pace_fps: 60,
        };
        let unit = backend
            .capture_video(&desktop(), crate::VideoCodec::H264, true, rate)
            .unwrap()
            .unwrap();
        assert_eq!(unit.format.gop, u32::MAX);
        drop(backend);
        helper.join().unwrap();
    }
}
