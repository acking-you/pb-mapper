use super::*;
use crate::{
    Display,
    outgoing::HostUnit,
    protocol::{HostMessage, NoticeCode, SessionState, VideoSize},
    shared::{CursorReport, CursorUpdate, EngineState},
};
use tokio::io::{AsyncReadExt, DuplexStream, ReadHalf, WriteHalf};

fn streaming(generation: u64) -> EngineState {
    let mut engine = EngineState::connecting(true);
    engine.record = StateRecord {
        state: SessionState::Streaming,
        generation,
        display: Some(Display {
            id: 1,
            name: "test".into(),
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        }),
        video: Some(VideoSize {
            width: 64,
            height: 64,
        }),
        ..engine.record
    };
    engine.encoder_dwell = Some(Duration::from_millis(3));
    engine
}

fn unit(generation: u64, frame_id: u64, keyframe: bool) -> HostUnit {
    HostUnit {
        generation,
        frame_id,
        codec: VideoCodec::H264,
        width: 64,
        height: 64,
        keyframe,
        duplicate: false,
        pts_us: frame_id * 16_667,
        encode_us: 0,
        ready_at: Instant::now(),
        bytes: bytes::Bytes::from(vec![frame_id as u8; 100]),
    }
}

/// A scripted viewer over an in-memory stream.
struct Viewer {
    read: ReadHalf<DuplexStream>,
    write: WriteHalf<DuplexStream>,
}
impl Viewer {
    async fn send(&mut self, message: ViewerMessage) {
        wire::write_message(&mut self.write, &message, None)
            .await
            .unwrap();
    }

    /// The next message other than a probe or stats, with its payload.
    async fn next(&mut self) -> (HostMessage, Option<bytes::Bytes>) {
        loop {
            let message = wire::read_header(&mut self.read, Duration::from_secs(5))
                .await
                .unwrap()
                .unwrap();
            match message {
                HostMessage::Probe { .. } | HostMessage::Stats { .. } => {}
                HostMessage::Video { .. } => {
                    let payload = wire::read_payload(&mut self.read).await.unwrap();
                    return (message, Some(payload));
                }
                message => return (message, None),
            }
        }
    }

    /// The next `Stats`, skipping everything else.
    async fn stats(&mut self) -> HostMessage {
        loop {
            let message = wire::read_header(&mut self.read, Duration::from_secs(5))
                .await
                .unwrap()
                .unwrap();
            match message {
                HostMessage::Stats { .. } => return message,
                HostMessage::Video { .. } => {
                    wire::read_payload(&mut self.read).await.unwrap();
                }
                _ => {}
            }
        }
    }
}

fn video(message: &HostMessage) -> (u64, u64, u64) {
    match message {
        HostMessage::Video {
            seq,
            generation,
            frame_id,
            ..
        } => (*seq, *generation, *frame_id),
        other => panic!("expected video, got {other:?}"),
    }
}

fn state_generation(message: &HostMessage) -> u64 {
    match message {
        HostMessage::State(record) => record.generation,
        other => panic!("expected state, got {other:?}"),
    }
}

#[tokio::test]
async fn stale_generation_units_are_dropped_before_admission() {
    let (viewer, host) = tokio::io::duplex(1 << 20);
    let (read, write) = tokio::io::split(viewer);
    let mut viewer = Viewer { read, write };
    let session = Session::new(
        Arc::new(Policy::new(true)),
        CancellationToken::new(),
        VideoCodec::H264,
        VideoLimits::for_viewer(60.0),
        QualityPreset::Balanced,
        &[],
        false,
    );
    let shared = session.shared.clone();
    shared.state.send_replace(streaming(1));
    let cancel = shared.cancel.clone();
    let running = tokio::spawn(run(session.clone(), host, async move {
        cancel.cancelled().await;
        WorkerExit::Cancelled
    }));

    assert_eq!(state_generation(&viewer.next().await.0), 1);
    shared.outgoing.push(unit(1, 1, true));
    let (first, payload) = viewer.next().await;
    assert_eq!(video(&first), (1, 1, 1));
    assert_eq!(payload.unwrap()[..], [1; 100]);

    // A control grant invalidates generation 1 while two of its units
    // wait in the hand-off: they are dropped without a seq. The new
    // generation's cursor waits for its `State`.
    let report = |serial| {
        Some(CursorReport {
            serial,
            cursor: CursorUpdate {
                generation: 2,
                x: 5,
                y: 6,
                visible: true,
            },
            input_sequence: 0,
        })
    };
    let cursor = |message: &HostMessage| {
        matches!(
            message,
            HostMessage::Cursor {
                generation: 2,
                x: 5,
                y: 6,
                visible: true,
                ..
            }
        )
    };
    shared.outgoing.push(unit(1, 2, false));
    shared.outgoing.push(unit(1, 3, false));
    shared.cursor.send_replace(report(1));
    shared.state.send_replace(streaming(2));
    shared.outgoing.push(unit(2, 4, true));
    assert_eq!(state_generation(&viewer.next().await.0), 2);
    assert!(cursor(&viewer.next().await.0));
    assert_eq!(video(&viewer.next().await.0), (2, 2, 4));

    // The session continues, and the seq stays contiguous. Each report
    // goes out once, even at a position written before.
    viewer.send(ViewerMessage::Received { seq: 2 }).await;
    shared.cursor.send_replace(report(2));
    shared.outgoing.push(unit(2, 5, false));
    assert!(cursor(&viewer.next().await.0));
    assert_eq!(video(&viewer.next().await.0), (3, 2, 5));
    let HostMessage::Stats(stats) = viewer.stats().await else {
        unreachable!()
    };
    assert_eq!((stats.dropped_frames, stats.frames), (2, 3));
    // A viewer that did not list `encoder_dwell` would reject the field.
    assert_eq!(stats.encoder_dwell_ms, None);

    // A receipt for video never sent is a protocol error, and the viewer
    // hears so before the close.
    viewer.send(ViewerMessage::Received { seq: 9 }).await;
    let (stopped, _) = viewer.next().await;
    assert!(
        matches!(
            stopped,
            HostMessage::Stopped {
                reason: StopReason::ProtocolError,
                ..
            }
        ),
        "{stopped:?}"
    );
    let mut rest = vec![];
    viewer.read.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
    drop(viewer);
    running.await.unwrap();
}

/// C publishes `streaming` and its first unit while B-send is in the
/// middle of a pass that already wrote `connecting`. The unit still
/// follows its own `State`: a viewer that saw it behind `connecting`
/// would wait for another IDR, its decoder recovering.
#[tokio::test]
async fn a_unit_follows_the_state_it_was_published_with() {
    // A stream this small makes every write wait for the viewer's read,
    // so the pass stops in its cursor, between its `State` and its video.
    let (viewer, host) = tokio::io::duplex(16);
    let (read, write) = tokio::io::split(viewer);
    let mut viewer = Viewer { read, write };
    let session = Session::new(
        Arc::new(Policy::new(true)),
        CancellationToken::new(),
        VideoCodec::H264,
        VideoLimits::for_viewer(60.0),
        QualityPreset::Balanced,
        &[],
        false,
    );
    let shared = session.shared.clone();
    shared.cursor.send_replace(Some(CursorReport {
        serial: 1,
        cursor: CursorUpdate {
            generation: 1,
            x: 5,
            y: 6,
            visible: true,
        },
        input_sequence: 0,
    }));
    let cancel = shared.cancel.clone();
    let running = tokio::spawn(run(session.clone(), host, async move {
        cancel.cancelled().await;
        WorkerExit::Cancelled
    }));
    let state = |message: HostMessage| match message {
        HostMessage::State(record) => record.state,
        other => panic!("expected state, got {other:?}"),
    };

    assert_eq!(state(viewer.next().await.0), SessionState::Connecting);
    shared.state.send_replace(streaming(1));
    shared.outgoing.push(unit(1, 1, true));
    assert!(matches!(
        viewer.next().await.0,
        HostMessage::Cursor { generation: 1, .. }
    ));
    assert_eq!(state(viewer.next().await.0), SessionState::Streaming);
    assert_eq!(video(&viewer.next().await.0), (1, 1, 1));

    shared.cancel.cancel();
    drop(viewer);
    running.await.unwrap();
}

/// A cursor report carries the input watermark only to a viewer that
/// listed `cursor_sequence`: an older one rejects a field it does not know.
#[tokio::test]
async fn the_cursor_watermark_goes_only_to_a_viewer_that_reads_it() {
    for listed in [false, true] {
        let (viewer, host) = tokio::io::duplex(1 << 20);
        let (read, write) = tokio::io::split(viewer);
        let mut viewer = Viewer { read, write };
        let capabilities = if listed {
            vec![CAPABILITY_CURSOR_SEQUENCE.to_owned()]
        } else {
            vec![]
        };
        let session = Session::new(
            Arc::new(Policy::new(true)),
            CancellationToken::new(),
            VideoCodec::H264,
            VideoLimits::for_viewer(60.0),
            QualityPreset::Balanced,
            &capabilities,
            false,
        );
        let shared = session.shared.clone();
        shared.state.send_replace(streaming(1));
        shared.cursor.send_replace(Some(CursorReport {
            serial: 1,
            cursor: CursorUpdate {
                generation: 1,
                x: 5,
                y: 6,
                visible: true,
            },
            input_sequence: 9,
        }));
        let cancel = shared.cancel.clone();
        let running = tokio::spawn(run(session, host, async move {
            cancel.cancelled().await;
            WorkerExit::Cancelled
        }));
        assert_eq!(state_generation(&viewer.next().await.0), 1);
        let (cursor, _) = viewer.next().await;
        assert!(
            matches!(
                cursor,
                HostMessage::Cursor { input_sequence, .. }
                    if input_sequence == listed.then_some(9)
            ),
            "{cursor:?}"
        );
        drop(viewer);
        running.await.unwrap();
    }
}

/// A viewer that did not list `display_mode` gets a `State` a stage6
/// viewer's strict record parses (no modes; a display still changing is
/// the capture failure it is) and never the display mode notice; a
/// request it sends anyway breaks the protocol. One that listed it gets
/// all of it, and its request reaches thread C.
#[tokio::test]
async fn display_modes_reach_only_a_viewer_that_listed_them() {
    /// What a stage6 viewer accepted as a `State`, as strictly.
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[expect(
        dead_code,
        reason = "it mirrors what a stage6 viewer required; the test reads only `reason`"
    )]
    struct Stage6Record {
        t: String,
        state: SessionState,
        #[serde(default)]
        reason: Option<Stage6Reason>,
        message: String,
        control_allowed: bool,
        controlling: bool,
        generation: u64,
        #[serde(default)]
        display: Option<Display>,
        desktop: crate::DesktopKind,
        #[serde(default)]
        video: Option<VideoSize>,
    }
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Stage6Reason {
        SecureDesktop,
        HelperUnavailable,
        CaptureFailed,
        InputFailed,
        ControlDenied,
        InputCongestion,
        StalePresentation,
        DisplayLost,
    }
    let mode = |width, height| Mode { width, height };
    for listed in [false, true] {
        let (viewer, host) = tokio::io::duplex(1 << 20);
        let (mut read, mut write) = tokio::io::split(viewer);
        let capabilities = if listed {
            vec![CAPABILITY_DISPLAY_MODE.to_owned()]
        } else {
            vec![]
        };
        let session = Session::new(
            Arc::new(Policy::new(true)),
            CancellationToken::new(),
            VideoCodec::H264,
            VideoLimits::for_viewer(60.0),
            QualityPreset::Balanced,
            &capabilities,
            true,
        );
        let shared = session.shared.clone();
        let mut engine = streaming(1);
        engine.record.state = SessionState::CaptureUnavailable;
        engine.record.reason = Some(StateReason::DisplayChanging);
        engine.record.modes = Some(crate::ModeList {
            current: mode(1920, 1080),
            original: mode(2560, 1440),
            supported: vec![mode(2560, 1440), mode(1920, 1080)],
        });
        engine.notice = Some((NoticeCode::DisplayModeFailed, "still changing".into(), 1));
        shared.state.send_replace(engine);
        let cancel = shared.cancel.clone();
        let running = tokio::spawn(run(session, host, async move {
            cancel.cancelled().await;
            WorkerExit::Cancelled
        }));
        // Everything up to the first `Stats`, as JSON.
        let mut seen = Vec::new();
        loop {
            let message: serde_json::Value = wire::read_header(&mut read, Duration::from_secs(5))
                .await
                .unwrap()
                .unwrap();
            if message["t"] == "stats" {
                break;
            }
            seen.push(message);
        }
        let state = seen.iter().find(|m| m["t"] == "state").unwrap();
        let notice = seen.iter().any(|m| m["t"] == "notice");
        if listed {
            assert_eq!(state["reason"], "display_changing");
            assert_eq!(state["modes"]["original"]["width"], 2560);
            assert!(notice);
        } else {
            let stage6: Stage6Record = serde_json::from_value(state.clone()).unwrap();
            assert_eq!(stage6.reason, Some(Stage6Reason::CaptureFailed));
            assert!(!notice);
        }

        let request = ViewerMessage::DisplayMode {
            width: 1920,
            height: 1080,
        };
        wire::write_message(&mut write, &request, None)
            .await
            .unwrap();
        if listed {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !shared.lane.drain().iter().any(|(command, _)| {
                    matches!(
                        command,
                        LaneCommand::DisplayMode(ModeCommand::Set(set)) if *set == mode(1920, 1080)
                    )
                }) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            shared.cancel.cancel();
        } else {
            let stopped = loop {
                let message: HostMessage = wire::read_header(&mut read, Duration::from_secs(5))
                    .await
                    .unwrap()
                    .unwrap();
                if let HostMessage::Stopped { reason, .. } = message {
                    break reason;
                }
                if let HostMessage::Video { .. } = message {
                    wire::read_payload(&mut read).await.unwrap();
                }
            };
            assert_eq!(stopped, StopReason::ProtocolError);
        }
        drop((read, write));
        running.await.unwrap();
    }
}

/// A viewer still writing when the host stops it hears why. Input that
/// reaches a closed socket resets the connection, and the reset discards
/// a `Stopped` the viewer has not read yet; so the host reads on until the
/// viewer closes.
#[tokio::test]
async fn stopped_survives_input_sent_after_the_stop() {
    use tokio::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut viewer = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (host, _) = listener.accept().await.unwrap();
    let session = Session::new(
        Arc::new(Policy::new(false)),
        CancellationToken::new(),
        VideoCodec::Jpeg,
        VideoLimits::for_viewer(60.0),
        QualityPreset::Balanced,
        &[],
        false,
    );
    let cancel = session.shared.cancel.clone();
    let running = tokio::spawn(run(session.clone(), host, async move {
        cancel.cancelled().await;
        WorkerExit::Cancelled
    }));
    session.end(StopReason::Revoked, stop_message(StopReason::Revoked));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = wire::write_message(&mut viewer, &ViewerMessage::Ping {}, None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    loop {
        match wire::read_header(&mut viewer, Duration::from_secs(1)).await {
            Ok(Some(HostMessage::Stopped { reason, .. })) => {
                assert_eq!(reason, StopReason::Revoked);
                break;
            }
            Ok(Some(_)) => {}
            other => panic!("closed without Stopped: {other:?}"),
        }
    }
    drop(viewer);
    running.await.unwrap();
}
