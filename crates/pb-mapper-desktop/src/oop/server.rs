//! The out-of-process backend's worker side: each request, served on the
//! worker's own backend, and the request loop over the transport. The
//! worker checks again what the host's thread C already checked: the input
//! sequence, and the whole desktop the host expects, at the injection
//! boundary.
use super::{
    ipc,
    proto::{DRAIN_WAIT_MAX_MS, Request, Response, ShapeHeader, TIMEOUT},
};
use crate::{Backend, CursorShape, DesktopContext, Result, error, failure_kind};

/// What the worker keeps across requests.
pub(crate) struct ServeState {
    /// The last input sequence applied.
    pub sequence: u64,
    /// When input was last applied or a presentation renewed it.
    pub last_input: std::time::Instant,
    /// The host asked for the pointer's shape: it is read with every pointer
    /// observed from then on. Never before, so a session whose viewer draws
    /// no shapes costs nothing here.
    shapes: bool,
    /// The newest shape read, and how many different ones were.
    shape: Option<CursorShape>,
    shape_serial: u64,
}
impl ServeState {
    pub(crate) fn new() -> Self {
        Self {
            sequence: 0,
            last_input: std::time::Instant::now(),
            shapes: false,
            shape: None,
            shape_serial: 0,
        }
    }
}

/// Hand the host's requests to the desktop-bound thread, one at a time, until
/// `Close`. Each answer is bounded by [`Request::timeout`], but the wait for the next
/// request is not: a host whose viewer is paused sends nothing for as long as
/// the pause lasts. The pipe closing ends that wait, and the service closes
/// it when its caller's pipe closes, or when the owner or its login ends.
pub(crate) async fn exchange<P, A>(
    pipe: &mut P,
    display_id: u32,
    display_device: Option<String>,
    mut serve: impl FnMut(Request) -> A,
) -> Result<()>
where
    P: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    A: std::future::Future<Output = Result<(Response, Vec<u8>)>>,
{
    let mut request = Request::Start {
        display_id,
        display_device,
    };
    loop {
        let close = matches!(request, Request::Close {});
        let (response, image) = tokio::time::timeout(request.timeout(), serve(request))
            .await
            .map_err(std::io::Error::other)??;
        tokio::time::timeout(TIMEOUT, async {
            ipc::write_json(pipe, &response).await?;
            ipc::write_bytes(pipe, &image, ipc::MAX_FRAME).await
        })
        .await
        .map_err(std::io::Error::other)??;
        if close {
            return Ok(());
        }
        request = ipc::read_json(pipe).await?;
    }
}

impl Response {
    /// The answer to a request that failed, with the failure's kind.
    pub(crate) fn failed(failure: &std::io::Error, pending: u32) -> Self {
        Self {
            error: Some(failure.to_string()),
            failure: failure_kind(failure),
            pending,
            ..Self::default()
        }
    }
}

/// One request on the worker's desktop-bound thread. The host's thread C
/// paces and authorises; this checks the input sequence again, and the
/// backend the whole desktop the host expects, at the injection boundary.
pub(crate) fn serve<B: Backend>(
    backend: &mut B,
    request: Request,
    state: &mut ServeState,
) -> Result<(Response, Vec<u8>)> {
    let mut response = Response::default();
    if matches!(request, Request::Release {} | Request::Close {}) {
        backend.release();
        return Ok((response, vec![]));
    }
    // Whatever desktop is bound now: what this worker changed goes back.
    if let Request::RestoreDisplayMode {} = request {
        response.restored = Some(backend.restore_display_mode()?);
        return Ok((response, vec![]));
    }
    if let Request::Input {
        expected,
        sequence: next,
        input,
    } = request
    {
        if next <= state.sequence {
            backend.release();
            return Err(error("repeated helper input"));
        }
        state.sequence = next;
        // Binds this thread to the input desktop, and fails unless it is
        // still `expected`.
        backend.input_in_context(&expected, &input)?;
        state.last_input = std::time::Instant::now();
        // The pointer as the input left it, so that the host's thread C knows
        // its own move when it comes back. The input stands without it.
        response.cursor = backend.cursor(&expected).ok().flatten();
        // The host keeps the desktop each answer reports: this is the one the
        // input reached.
        response.context = Some(expected);
        return Ok((response, vec![]));
    }
    let context = backend.context()?;
    // Context checks observe the pointer too, so it follows while capture is
    // idle. The backend checks this desktop identity; observing it never
    // renews last_input or acknowledges video.
    let observe_cursor = request.observes_cursor();
    let answer_shape = matches!(request, Request::CursorShape {});
    let mut image = match request {
        Request::Start { .. } => {
            // This worker runs in the interactive session: its settings are
            // the user's, which the service in session 0 cannot read. What
            // it can do is said at once, never H.265, which costs a probe.
            response.double_click = backend.double_click_guard();
            response.caps = Some((&backend.describe(false)?.caps).into());
            vec![]
        }
        Request::Context {} => vec![],
        // Answered above, before the context read.
        Request::Release {}
        | Request::Close {}
        | Request::Input { .. }
        | Request::RestoreDisplayMode {} => vec![],
        Request::DisplayModes { expected } => {
            if expected != context {
                return Err(error("stale display mode request"));
            }
            response.modes = backend.display_modes(&expected)?;
            vec![]
        }
        Request::SetDisplayMode { expected, mode } => {
            // The backend checks the bound desktop is still `expected`.
            backend.set_display_mode(&expected, mode)?;
            vec![]
        }
        Request::Attach { .. } => return Err(error("worker is already attached")),
        Request::H265 {} => {
            response.h265 = Some(
                backend
                    .describe(true)
                    .is_ok_and(|described| described.caps.h265),
            );
            vec![]
        }
        Request::ConfigureVideo {
            limits,
            nominal_fps,
        } => {
            if !(1..=crate::video::MAX_FPS).contains(&nominal_fps) {
                return Err(error("invalid video configuration"));
            }
            backend.configure_video(limits, nominal_fps)?;
            vec![]
        }
        Request::Capture {} => backend.capture_consistent(&context)?,
        Request::Video {
            expected,
            codec,
            submit,
            keyframe,
            rate,
            wait_ms,
        } => {
            if expected != context
                || wait_ms > DRAIN_WAIT_MAX_MS
                || submit
                    && (!(crate::qos::ENCODER_BITRATE_MIN..=crate::qos::ENCODER_BITRATE_MAX)
                        .contains(&rate.bitrate)
                        || !(1..=crate::video::MAX_FPS).contains(&rate.pace_fps))
            {
                return Err(error("invalid or stale video request"));
            }
            let frame = if submit {
                backend.capture_video(&expected, codec, keyframe, rate)?
            } else {
                drain(
                    backend,
                    &expected,
                    std::time::Duration::from_millis(wait_ms.into()),
                )?
            };
            match frame {
                Some(frame) => {
                    response.video = Some(frame.format);
                    frame.bytes
                }
                None => vec![],
            }
        }
        Request::Presented { generation } => {
            if generation != context.generation {
                return Err(error("stale helper presentation"));
            }
            state.last_input = std::time::Instant::now();
            vec![]
        }
        // Answered below, once the pointer is observed.
        Request::CursorShape {} => {
            state.shapes = true;
            vec![]
        }
    };
    if observe_cursor {
        response.cursor = backend.cursor(&context)?;
        if state.shapes {
            // As thread C asks: only of a pointer showing on the display
            // shared.
            if response.cursor.is_some_and(|cursor| cursor.visible) {
                observe_shape(backend, &context, state)?;
            }
            response.shape_serial = Some(state.shape_serial);
        }
    }
    if answer_shape && let Some(shape) = &state.shape {
        response.shape = Some(ShapeHeader::of(shape));
        image = shape.rgba().to_vec();
    }
    response.context = Some(context);
    Ok((response, image))
}

/// Read the pointer's shape into `state`, numbering a new one. A shape the
/// backend could not read keeps the last; only a lost link fails.
fn observe_shape<B: Backend>(
    backend: &mut B,
    context: &DesktopContext,
    state: &mut ServeState,
) -> Result<()> {
    match backend.cursor_shape(context) {
        Ok(Some(shape)) if state.shape.as_ref() != Some(&shape) => {
            state.shape = Some(shape);
            state.shape_serial += 1;
        }
        Ok(_) => {}
        Err(failure) if failure.kind() == std::io::ErrorKind::NotConnected => {
            return Err(failure);
        }
        Err(failure) => tracing::debug!(
            target: "pb_mapper_desktop::session",
            event = "desktop_cursor_shape_failed",
            detail = %failure,
        ),
    }
    Ok(())
}

/// Drain one unit, waiting up to `wait` while the encoder still holds units,
/// so the host polls a busy encoder without a pipe round trip per poll.
fn drain<B: Backend>(
    backend: &mut B,
    expected: &DesktopContext,
    wait: std::time::Duration,
) -> Result<Option<crate::EncodedFrame>> {
    let until = std::time::Instant::now() + wait;
    loop {
        if let Some(frame) = backend.drain_video(expected, std::time::Duration::ZERO)? {
            return Ok(Some(frame));
        }
        if backend.pending_video() == 0 || std::time::Instant::now() >= until {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Input;

    #[test]
    fn input_reaches_only_the_desktop_the_host_expected() {
        /// The bound desktop, which the test changes under the host.
        struct Bound {
            context: DesktopContext,
            injected: u32,
        }
        impl Backend for Bound {
            fn display(&mut self) -> Result<crate::Display> {
                Ok(self.context.display.clone())
            }
            fn context(&mut self) -> Result<DesktopContext> {
                Ok(self.context.clone())
            }
            fn capture(&mut self) -> Result<Vec<u8>> {
                Ok(vec![])
            }
            fn input(&mut self, _: &Input) -> Result<()> {
                self.injected += 1;
                Ok(())
            }
            fn release(&mut self) {}
        }
        let expected = DesktopContext {
            display: crate::Display {
                id: 1,
                name: "bound".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            generation: 1,
            desktop: crate::DesktopKind::Ordinary,
        };
        let mut backend = Bound {
            context: expected.clone(),
            injected: 0,
        };
        let mut state = ServeState::new();
        let mut send = |backend: &mut Bound, next| {
            let request = Request::Input {
                expected: expected.clone(),
                sequence: next,
                input: Input::Move { x: 1, y: 1 },
            };
            serve(backend, request, &mut state).map(|(response, _)| response)
        };
        // The host keeps the desktop an answer reports, for its pointer.
        let response = send(&mut backend, 1).unwrap();
        assert_eq!(response.context, Some(expected.clone()));
        assert!(send(&mut backend, 1).is_err(), "a repeated sequence");
        // The same generation on a resized display is another desktop.
        backend.context.display.width = 32;
        assert!(send(&mut backend, 2).is_err());
        assert_eq!(backend.injected, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_worker_waits_out_a_paused_host() {
        async fn answer(host: &mut tokio::io::DuplexStream) -> Result<Response> {
            let response = ipc::read_json(host).await?;
            ipc::read_bytes(host, ipc::MAX_FRAME).await?;
            Ok(response)
        }
        let (mut host, mut worker) = tokio::io::duplex(1 << 16);
        let exchange = tokio::spawn(async move {
            exchange(&mut worker, 1, None, |_| async {
                Ok((Response::default(), vec![]))
            })
            .await
        });
        answer(&mut host).await.unwrap();
        // The viewer stays paused for a minute: its host sends nothing.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        ipc::write_json(&mut host, &Request::Context {})
            .await
            .unwrap();
        answer(&mut host).await.unwrap();
        ipc::write_json(&mut host, &Request::Close {})
            .await
            .unwrap();
        answer(&mut host).await.unwrap();
        exchange.await.unwrap().unwrap();
    }

    /// A worker builds its backend apart from any session, and no thread C
    /// binds it: its video must still encode.
    #[test]
    fn a_worker_backend_encodes_without_a_session() {
        use crate::{
            VideoCodec, VideoLimits, VideoRate,
            native::PlatformBackend,
            platform::{
                Platform,
                synthetic::{Probe, Synthetic},
            },
        };
        let probe = Probe::default();
        let parts = Synthetic::<false>::open(&probe, &Probe::display()).unwrap();
        let mut backend = PlatformBackend::from_parts(
            std::sync::Arc::new(probe),
            parts,
            crate::modes::Journal::default(),
        );
        let mut state = ServeState::new();
        let mut serve = |request| serve(&mut backend, request, &mut state).unwrap();
        let (started, _) = serve(Request::Start {
            display_id: 1,
            display_device: None,
        });
        serve(Request::ConfigureVideo {
            limits: VideoLimits::for_viewer(60.0),
            nominal_fps: 60,
        });
        let video = |keyframe| Request::Video {
            expected: started.context.clone().unwrap(),
            codec: VideoCodec::H264,
            submit: true,
            keyframe,
            rate: VideoRate {
                bitrate: crate::qos::ENCODER_BITRATE_MIN,
                pace_fps: 60,
            },
            wait_ms: 0,
        };
        // The first submit fills the encoder; the next drains its unit.
        let (first, _) = serve(video(true));
        assert!(first.error.is_none() && first.video.is_none());
        let (second, bytes) = serve(video(false));
        assert_eq!(second.video.unwrap().codec, VideoCodec::H264);
        assert!(!bytes.is_empty());
    }
}
