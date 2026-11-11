use super::{session::VIEWER_TICK, *};
use crate::{
    Display, Key, VideoLimits,
    protocol::{
        CAPABILITY_CURSOR_SEQUENCE, CAPABILITY_ENCODER_DWELL, CAPABILITY_FPS_OVERRIDE,
        CAPABILITY_H265, HostMessage, RejectReason, StopReason, ViewerMessage,
    },
    wire,
};
use std::{
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};
use tokio::io::{AsyncWriteExt, DuplexStream, ReadBuf};

const TOKEN: &str = "0123456789012345678901234567890123456789012";

fn config() -> ViewerConfig {
    ViewerConfig {
        codecs: vec![VideoCodec::H264],
        limits: VideoLimits::for_viewer(60.0),
        fps_override: false,
        quality: QualityPreset::Balanced,
        progress: None,
    }
}

async fn send(host: &mut DuplexStream, message: HostMessage, payload: Option<&[u8]>) {
    wire::write_message(host, &message, payload).await.unwrap();
}

/// The viewer's next message other than a ping.
async fn receive(host: &mut DuplexStream) -> ViewerMessage {
    loop {
        match wire::read_header(host, Duration::from_secs(5))
            .await
            .unwrap()
        {
            Some(ViewerMessage::Ping {}) => {}
            Some(message) => return message,
            None => panic!("the viewer closed the stream"),
        }
    }
}

/// Read until a message matches, returning it.
async fn until(
    host: &mut DuplexStream,
    mut matches: impl FnMut(&ViewerMessage) -> bool,
) -> ViewerMessage {
    loop {
        let message = receive(host).await;
        if matches(&message) {
            return message;
        }
    }
}

fn state(generation: u64, controlling: bool) -> HostMessage {
    HostMessage::State(StateRecord {
        state: SessionState::Streaming,
        reason: None,
        message: String::new(),
        control_allowed: true,
        controlling,
        generation,
        display: Some(Display {
            id: 1,
            name: "scripted".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }),
        desktop: DesktopKind::Ordinary,
        video: Some(VideoSize {
            width: 1920,
            height: 1080,
        }),
        modes: None,
    })
}

fn video(seq: u64, generation: u64, frame_id: u64, keyframe: bool) -> HostMessage {
    HostMessage::Video {
        seq,
        generation,
        frame_id,
        keyframe,
        duplicate: false,
        width: 1920,
        height: 1080,
        pts_us: 0,
        host_us: 2_000,
    }
}

async fn pull(consumer: &crate::VideoConsumer) -> VideoUnit {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match consumer.next(Duration::ZERO, 1 << 20) {
                Pull::Unit(unit) => return unit,
                Pull::Timeout => tokio::time::sleep(Duration::from_millis(5)).await,
                other => panic!("expected a unit, got {other:?}"),
            }
        }
    })
    .await
    .unwrap()
}

/// Poll `condition` for up to 5 s.
async fn eventually(condition: impl FnMut() -> bool) {
    crate::test_util::until_within(Duration::from_secs(5), condition).await;
}

fn accepted() -> HostMessage {
    accepted_with(VideoCodec::H264)
}

fn accepted_with(codec: VideoCodec) -> HostMessage {
    HostMessage::Accepted {
        codec,
        limits: VideoLimits::for_viewer(60.0),
        nominal_fps: 60,
        quality: QualityPreset::Balanced,
        capabilities: vec!["lock".into()],
    }
}

/// Connect over a duplex stream; the host side answers `answer`.
async fn connect(
    answer: HostMessage,
) -> (
    std::result::Result<Viewer, ConnectError>,
    DuplexStream,
    ViewerMessage,
) {
    connect_with(config(), answer).await
}

async fn connect_with(
    config: ViewerConfig,
    answer: HostMessage,
) -> (
    std::result::Result<Viewer, ConnectError>,
    DuplexStream,
    ViewerMessage,
) {
    let (viewer, mut host) = tokio::io::duplex(1 << 20);
    let connecting = tokio::spawn(Viewer::start(viewer, TOKEN.into(), config, None));
    let authenticate = receive(&mut host).await;
    send(&mut host, answer, None).await;
    (connecting.await.unwrap(), host, authenticate)
}

#[tokio::test]
async fn scripted_host_session_acknowledges_first_and_authorises_input_by_presentation() {
    let rejected = async |reason| connect(HostMessage::Rejected { reason }).await.0.err();
    assert!(matches!(
        rejected(RejectReason::Busy).await,
        Some(ConnectError::Busy)
    ));
    assert!(matches!(
        rejected(RejectReason::Denied).await,
        Some(ConnectError::Denied)
    ));
    assert!(matches!(
        rejected(RejectReason::InvalidRequest).await,
        Some(ConnectError::InvalidRequest)
    ));

    let (viewer, mut host, authenticate) = connect(accepted()).await;
    let viewer = viewer.unwrap();
    let ViewerMessage::Authenticate {
        token,
        codecs,
        capabilities,
        ..
    } = authenticate
    else {
        panic!("expected Authenticate first");
    };
    assert_eq!(token, TOKEN);
    assert_eq!(codecs, [VideoCodec::H264]);
    assert_eq!(
        capabilities,
        [
            CAPABILITY_ENCODER_DWELL,
            CAPABILITY_CURSOR_SEQUENCE,
            CAPABILITY_DISPLAY_MODE
        ]
    );

    // Receipts and probe echoes need no consumer at all.
    send(&mut host, state(1, false), None).await;
    send(&mut host, HostMessage::Probe { nonce: 7 }, None).await;
    send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
    let (mut echoed, mut received) = (false, false);
    while !(echoed && received) {
        match receive(&mut host).await {
            ViewerMessage::ProbeEcho { nonce: 7 } => echoed = true,
            ViewerMessage::Received { seq: 1 } => received = true,
            other => panic!("unexpected {other:?}"),
        }
    }

    // Input waits for a presented frame and for control.
    let shift = [Input::Key {
        key: Key::Shift,
        down: true,
    }];
    assert!(matches!(viewer.input(1, &shift), Err(InputError::NotFresh)));
    let consumer = viewer.video_sink().attach().unwrap();
    let unit = pull(&consumer).await;
    assert!(unit.keyframe && unit.discontinuity);
    assert!(consumer.presented(1, 1));
    until(&mut host, |m| {
        matches!(
            m,
            ViewerMessage::Presented {
                generation: 1,
                frame_id: 1
            }
        )
    })
    .await;
    assert!(viewer.status().fresh);
    assert!(matches!(
        viewer.input(1, &shift),
        Err(InputError::NotControlling)
    ));
    viewer.request_control(true).unwrap();
    until(&mut host, |m| {
        matches!(m, ViewerMessage::Control { enabled: true })
    })
    .await;
    send(&mut host, state(1, true), None).await;
    eventually(|| viewer.status().controlling).await;
    assert!(matches!(viewer.input(2, &shift), Err(InputError::NotFresh)));
    viewer.input(1, &shift).unwrap();
    let ViewerMessage::Input { event } = receive(&mut host).await else {
        panic!("expected the input");
    };
    assert_eq!(
        (event.generation, event.frame_id, event.sequence),
        (1, 1, 1)
    );

    // A unit of an older generation is acknowledged and never shown.
    send(&mut host, state(2, false), None).await;
    send(&mut host, video(2, 1, 2, false), Some(&[2; 64])).await;
    until(&mut host, |m| {
        matches!(m, ViewerMessage::Received { seq: 2 })
    })
    .await;
    assert!(matches!(
        consumer.next(Duration::from_millis(20), 1 << 20),
        Pull::Timeout
    ));
    let status = viewer.status();
    assert!(!status.fresh && !status.controlling && status.generation == 2);

    // The host's own reason ends the session, and the consumer exits.
    send(
        &mut host,
        HostMessage::Stopped {
            reason: StopReason::HostEnded,
            message: "ended by the host".into(),
        },
        None,
    )
    .await;
    eventually(|| viewer.status().state == ViewerState::Stopped).await;
    let status = viewer.status();
    assert_eq!(status.reason, Some(ViewerReason::HostEnded));
    assert_eq!(status.message, "ended by the host");
    assert!(matches!(
        consumer.next(Duration::ZERO, 1 << 20),
        Pull::Closed
    ));
    assert!(matches!(viewer.input(2, &shift), Err(InputError::Closed)));
    viewer.stop().await.unwrap();
}

/// H.265 and a chosen frame rate are capabilities, so an older host sees
/// only H.264 and values it ignores; H.265 is taken only when offered.
#[tokio::test]
async fn h265_is_offered_as_a_capability_and_accepted_only_when_offered() {
    let h265 = || ViewerConfig {
        codecs: vec![VideoCodec::H265, VideoCodec::H264],
        fps_override: true,
        ..config()
    };
    let (viewer, mut host, authenticate) =
        connect_with(h265(), accepted_with(VideoCodec::H265)).await;
    let viewer = viewer.unwrap();
    let ViewerMessage::Authenticate {
        codecs,
        capabilities,
        ..
    } = authenticate
    else {
        panic!("expected Authenticate first");
    };
    assert_eq!(codecs, [VideoCodec::H264]);
    assert_eq!(
        capabilities,
        [
            CAPABILITY_ENCODER_DWELL,
            CAPABILITY_CURSOR_SEQUENCE,
            CAPABILITY_DISPLAY_MODE,
            CAPABILITY_H265,
            CAPABILITY_FPS_OVERRIDE
        ]
    );
    assert_eq!(viewer.status().stats.codec, VideoCodec::H265);
    send(&mut host, state(1, false), None).await;
    send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
    let consumer = viewer.video_sink().attach().unwrap();
    assert_eq!(pull(&consumer).await.codec, VideoCodec::H265);
    drop(host);
    viewer.stop().await.unwrap();

    let (unasked, _, _) = connect(accepted_with(VideoCodec::H265)).await;
    assert!(matches!(unasked, Err(ConnectError::Protocol(_))));
    // H.265 alone would leave an older host no codec it knows.
    let alone = ViewerConfig {
        codecs: vec![VideoCodec::H265],
        ..config()
    };
    let (stream, _host) = tokio::io::duplex(64);
    assert!(matches!(
        Viewer::over_stream_for_test(stream, TOKEN.into(), alone).await,
        Err(ConnectError::InvalidRequest)
    ));
}

/// The offer follows the decoders, and the limits the one the host picks
/// first: a phone whose H.265 decoder stops at 1080p, probed upright,
/// keeps H.265 at the default size. Without a decoder, JPEG.
#[test]
fn the_offer_and_limits_follow_the_decoders() {
    let native =
        |decoders| ViewerConfig::for_decoders(decoders, crate::Resolution::default(), 60.0, None);
    let phone = native(Decoders {
        h264: Some((3840, 2160)),
        h265: Some((1080, 1920)),
    });
    assert_eq!(phone.codecs, [VideoCodec::H265, VideoCodec::H264]);
    let bounds = |config: &ViewerConfig| (config.limits.max_width, config.limits.max_height);
    assert_eq!(bounds(&phone), (1920, 1080));
    let none = native(Decoders::default());
    assert!(
        none.capabilities()
            .contains(&CAPABILITY_DISPLAY_MODE.to_owned())
    );
    assert_eq!(
        (none.codecs.as_slice(), bounds(&none)),
        (&[VideoCodec::Jpeg][..], (3840, 2160))
    );
}

/// A host that did not list `display_mode` is never asked for a mode,
/// even one whose state carries modes; one that did is asked, live, and
/// only while this viewer controls it.
#[tokio::test]
async fn display_modes_are_asked_only_of_a_host_that_offers_them() {
    let mode = |width, height| Mode { width, height };
    for offered in [false, true] {
        let mut answer = accepted();
        if let HostMessage::Accepted { capabilities, .. } = &mut answer
            && offered
        {
            capabilities.push(CAPABILITY_DISPLAY_MODE.into());
        }
        let (viewer, mut host, _) = connect(answer).await;
        let viewer = viewer.unwrap();
        let with_modes = |controlling| {
            let HostMessage::State(mut record) = state(1, controlling) else {
                unreachable!()
            };
            record.modes = Some(ModeList {
                current: mode(1920, 1080),
                original: mode(1920, 1080),
                supported: vec![mode(1920, 1080), mode(1280, 720)],
            });
            HostMessage::State(record)
        };
        send(&mut host, with_modes(false), None).await;
        eventually(|| viewer.status().generation == 1).await;
        assert_eq!(viewer.status().modes.is_some(), offered);
        let smaller = ModeTarget::Size(mode(1280, 720));
        if !offered {
            assert_eq!(
                viewer.set_display_mode(smaller),
                Err(DisplayModeError::NotOffered)
            );
            continue;
        }
        assert_eq!(
            viewer.set_display_mode(smaller),
            Err(DisplayModeError::NotControlling)
        );
        viewer.request_control(true).unwrap();
        send(&mut host, with_modes(true), None).await;
        eventually(|| viewer.status().controlling).await;
        assert_eq!(
            viewer.set_display_mode(ModeTarget::Size(mode(800, 600))),
            Err(DisplayModeError::NotListed)
        );
        assert_eq!(viewer.set_display_mode(smaller), Ok(mode(1280, 720)));
        until(&mut host, |message| {
            matches!(
                message,
                ViewerMessage::DisplayMode {
                    width: 1280,
                    height: 720
                }
            )
        })
        .await;
        viewer.stop().await.unwrap();
    }
}

/// A report the host sampled before it took this viewer's last move is set
/// aside while this viewer controls: the host never reports that move's
/// echo, so the cursor would stay where the host's pointer is not. One at
/// or after the move is taken, and so is any once control is given up.
#[tokio::test]
async fn a_cursor_report_older_than_the_last_move_is_set_aside_while_controlling() {
    let (viewer, mut host, _) = connect(accepted()).await;
    let viewer = viewer.unwrap();
    send(&mut host, state(1, false), None).await;
    send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
    let consumer = viewer.video_sink().attach().unwrap();
    pull(&consumer).await;
    assert!(consumer.presented(1, 1));
    viewer.request_control(true).unwrap();
    until(&mut host, |m| {
        matches!(m, ViewerMessage::Control { enabled: true })
    })
    .await;
    send(&mut host, state(1, true), None).await;
    eventually(|| viewer.status().controlling).await;

    let cursor = |x, input_sequence| HostMessage::Cursor {
        generation: 1,
        x,
        y: 0,
        visible: true,
        input_sequence: Some(input_sequence),
    };
    let seen = || {
        let view = viewer.view();
        (view.cursor_seq, view.cursor_x)
    };
    // The first report of a generation is taken, however old.
    send(&mut host, cursor(10, 0), None).await;
    eventually(|| seen().1 == 10).await;
    let first = seen().0;
    viewer.input(1, &[Input::Move { x: 20, y: 0 }]).unwrap();
    let ViewerMessage::Input { event } =
        until(&mut host, |m| matches!(m, ViewerMessage::Input { .. })).await
    else {
        unreachable!()
    };
    send(&mut host, cursor(30, event.sequence - 1), None).await;
    send(&mut host, cursor(40, event.sequence), None).await;
    eventually(|| seen().1 == 40).await;
    assert_eq!(seen().0, first + 1, "the older report was taken");

    viewer.request_control(false).unwrap();
    send(&mut host, cursor(50, 0), None).await;
    eventually(|| seen().1 == 50).await;
    drop(host);
    viewer.stop().await.unwrap();
}

/// Dart reconnects after 5 s without video while streaming: neither
/// unavailable capture nor a pause may count, however long, before the
/// host's IDR arrives.
#[tokio::test]
async fn time_without_video_counts_only_while_streaming() {
    let (viewer, mut host, _) = connect(accepted()).await;
    let viewer = viewer.unwrap();
    send(&mut host, state(1, false), None).await;
    send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
    until(&mut host, |m| {
        matches!(m, ViewerMessage::Received { seq: 1 })
    })
    .await;
    let age_within = |due: Instant| {
        let status = viewer.status();
        assert_eq!(status.state, ViewerState::Streaming);
        assert!(status.last_video_age_ms.unwrap() <= saturating_millis(due.elapsed()));
    };

    let HostMessage::State(streaming) = state(1, false) else {
        unreachable!()
    };
    let unavailable = StateRecord {
        state: SessionState::CaptureUnavailable,
        ..streaming
    };
    send(&mut host, HostMessage::State(unavailable), None).await;
    eventually(|| viewer.status().state == ViewerState::CaptureUnavailable).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let due = Instant::now();
    send(&mut host, state(2, false), None).await;
    eventually(|| viewer.status().state == ViewerState::Streaming).await;
    age_within(due);

    viewer.pause();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let due = Instant::now();
    viewer.resume();
    age_within(due);
    drop(host);
    viewer.stop().await.unwrap();
}

/// The host's figures reach the status the quality sheet reads: its
/// skipped ticks as a rate, what it sent, its encoder dwell and receipt
/// queue, beside the preset and ceiling the session asked for.
#[tokio::test]
async fn host_stats_reach_the_viewer_status() {
    let (viewer, mut host, _) = connect(accepted()).await;
    let viewer = viewer.unwrap();
    let stats = |skipped_ticks, keyframes| {
        HostMessage::Stats(crate::protocol::StatsRecord {
            capture_fps: 59.5,
            pace_fps: 60,
            target_fps: 60,
            fps_cap: 0,
            bitrate: 4_000_000,
            encoder_bitrate: 4_000_000,
            sent_bitrate: 3_900_000,
            rtt_ms: Some(40),
            base_rtt_ms: Some(38),
            queue_delay_ms: Some(2),
            ack_queue_ms: 12,
            encoder_dwell_ms: Some(3),
            dropped_frames: 0,
            skipped_ticks,
            keyframes,
            frames: 60,
            bytes: 500_000,
            adaptation: crate::Adaptation::Steady,
            rate_model: crate::RateModel::Nominal,
            gop: u32::MAX,
        })
    };
    let keyframes = || viewer.status().stats.keyframes;
    send(&mut host, stats(10, 1), None).await;
    eventually(|| keyframes() == 1).await;
    // 30 more right behind it, as a link that held the first delivers
    // them: the host sent the two at least `STATS_INTERVAL` apart.
    send(&mut host, stats(40, 2), None).await;
    eventually(|| keyframes() == 2).await;
    let json = serde_json::to_value(viewer.status().stats).unwrap();
    let skipped = json["skipped_per_s"].as_f64().unwrap();
    assert!((15.0..=30.0).contains(&skipped), "{skipped}");
    assert_eq!(json["preset"], "balanced");
    assert_eq!(json["max_fps"], 60);
    assert_eq!(json["sent_bitrate"], 3_900_000);
    assert_eq!(json["encoder_dwell_ms"], 3);
    assert_eq!(json["ack_queue_ms"], 12);
    viewer.set_quality(QualityPreset::Smooth);
    assert_eq!(viewer.status().stats.preset, QualityPreset::Smooth);
    drop(host);
    viewer.stop().await.unwrap();
}

/// An IDR slower to cross than the hold after a new generation: its
/// header answers the hold, so nothing asks for another while its payload
/// is still in transit.
#[tokio::test]
async fn an_idr_slower_than_the_hold_is_not_asked_for_again() {
    let (viewer, mut host, _) = connect(accepted()).await;
    let viewer = viewer.unwrap();
    send(&mut host, state(1, false), None).await;
    // The header now, its payload record only once the hold has expired.
    send(&mut host, video(1, 1, 1, true), None).await;
    tokio::time::sleep(recovery::HOLD_AFTER_DISCONTINUITY + 3 * VIEWER_TICK).await;
    host.write_u32(64).await.unwrap();
    host.write_all(&[1; 64]).await.unwrap();
    until(&mut host, |m| {
        matches!(m, ViewerMessage::Received { seq: 1 })
    })
    .await;
    assert_eq!(viewer.status().stats.keyframes_requested, 0);
    drop(host);
    viewer.stop().await.unwrap();
}

/// A session task dropped mid-session, as its runtime shutting down
/// drops it, still ends the session: the consumer exits and the status
/// says stopped, with no reason, as a local stop does.
#[test]
fn a_session_dropped_with_its_runtime_ends_stopped() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (viewer, consumer, host) = runtime.block_on(async {
        let (viewer, mut host, _) = connect(accepted()).await;
        let viewer = viewer.unwrap();
        let consumer = viewer.video_sink().attach().unwrap();
        send(&mut host, state(1, false), None).await;
        eventually(|| viewer.status().generation == 1).await;
        (viewer, consumer, host)
    });
    drop(runtime);
    assert!(matches!(
        consumer.next(Duration::ZERO, 1 << 20),
        Pull::Closed
    ));
    let status = viewer.status();
    assert_eq!((status.state, status.reason), (ViewerState::Stopped, None));
    drop(host);
}

/// A stream whose reads panic once `armed`, as a bug in the session's code
/// would.
struct Faulty {
    inner: DuplexStream,
    armed: Arc<AtomicBool>,
}
impl AsyncRead for Faulty {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        assert!(!self.armed.load(Ordering::SeqCst), "a bug in the session");
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for Faulty {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// A session task that panics ends as a failure the supervisor's policy
/// retries, as it retried the stall such a panic used to leave behind; only
/// a drop that is not a panic reads as a local stop.
#[tokio::test]
async fn a_session_that_panics_ends_as_a_failure_the_supervisor_retries() {
    let (stream, mut host) = tokio::io::duplex(1 << 20);
    let armed = Arc::new(AtomicBool::new(false));
    let stream = Faulty {
        inner: stream,
        armed: armed.clone(),
    };
    let connecting = tokio::spawn(Viewer::start(stream, TOKEN.into(), config(), None));
    receive(&mut host).await;
    send(&mut host, accepted(), None).await;
    let viewer = connecting.await.unwrap().unwrap();
    let consumer = viewer.video_sink().attach().unwrap();
    send(&mut host, state(1, false), None).await;
    eventually(|| viewer.status().state == ViewerState::Streaming).await;

    armed.store(true, Ordering::SeqCst);
    send(&mut host, HostMessage::Probe { nonce: 1 }, None).await;
    eventually(|| viewer.status().state == ViewerState::Stopped).await;
    let status = viewer.status();
    assert_eq!(status.reason, Some(ViewerReason::Transport));
    assert!(matches!(
        consumer.next(Duration::ZERO, 1 << 20),
        Pull::Closed
    ));
    let mut policy = crate::supervisor::Reconnect::new(crate::supervisor::RetryPolicy::default());
    assert!(
        policy.after(&status, Instant::now()).is_some(),
        "a supervisor gives up on a panicked session"
    );
    drop(host);
    viewer.stop().await.unwrap();
}

#[tokio::test]
async fn a_decoder_that_cannot_recover_ends_the_session() {
    let (viewer, mut host, _) = connect(accepted()).await;
    let viewer = viewer.unwrap();
    send(&mut host, state(1, false), None).await;
    send(&mut host, video(1, 1, 1, true), Some(&[1; 64])).await;
    let consumer = viewer.video_sink().attach().unwrap();
    pull(&consumer).await;
    assert!(consumer.presented(1, 1));

    // The first error asks for an IDR; the fifth with no picture since
    // gives up, and the app is told why, in the decoder's words.
    let error = "Create H.264 decoder (0x80040154)";
    assert!(consumer.failed(error));
    for _ in 0..4 {
        assert!(!consumer.failed(error));
    }
    eventually(|| viewer.status().state == ViewerState::Stopped).await;
    let status = viewer.status();
    assert_eq!(status.reason, Some(ViewerReason::DecoderFailed));
    assert!(status.message.contains("0x80040154"), "{}", status.message);
    assert_eq!(status.decoder.state, DecoderState::Failed);
    assert!(matches!(
        consumer.next(Duration::ZERO, 1 << 20),
        Pull::Closed
    ));
    drop(host);
    viewer.stop().await.unwrap();
}
