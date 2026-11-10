use super::*;
use crate::{
    BackendCaps, QualityPreset, VideoCodec, VideoLimits,
    launch::Launcher,
    protocol::{CAPABILITY_H265, HostMessage, RejectReason, ViewerMessage},
    security, wire,
};
use pb_mapper_client::sdk::{ConnectOptions, Connection, PathPolicy};
use std::sync::atomic::AtomicU32;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientStream;

struct Screen;
impl crate::Backend for Screen {
    fn display(&mut self) -> Result<crate::Display> {
        Ok(crate::Display {
            id: 1,
            name: "trusted".into(),
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Ok(vec![1])
    }
    fn input(&mut self, _: &crate::Input) -> Result<()> {
        Err(error("read-only test backend"))
    }
    fn release(&mut self) {}
}
fn options() -> HostOptions {
    let mut options = HostOptions::new("desktop:remembered");
    options.allow_direct = false;
    options
}

/// A scripted v2 viewer: its TLS stream, and the tunnel it rides on.
struct Viewer {
    stream: ClientStream<TcpStream>,
    _connection: Connection,
}

/// A v2 viewer's TLS connection to `invitation`, pinned to its fingerprint.
async fn tls(client: &Client, invitation: &Invitation) -> std::io::Result<Viewer> {
    let (socket, connection) = client
        .connect_tcp(
            invitation.service.clone(),
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
        )
        .await
        .map_err(error)?;
    let stream = security::client(invitation.fingerprint)?
        .connect(
            quinn::rustls::pki_types::ServerName::try_from(security::NAME).unwrap(),
            socket,
        )
        .await?;
    Ok(Viewer {
        stream,
        _connection: connection,
    })
}

/// Authenticate with `token`; returns the viewer and the host's answer.
async fn authenticate(
    client: &Client,
    invitation: &Invitation,
    token: String,
    codecs: Vec<VideoCodec>,
) -> (Viewer, HostMessage) {
    authenticate_with(client, invitation, token, codecs, vec![]).await
}

/// `authenticate`, listing `capabilities`.
async fn authenticate_with(
    client: &Client,
    invitation: &Invitation,
    token: String,
    codecs: Vec<VideoCodec>,
    capabilities: Vec<String>,
) -> (Viewer, HostMessage) {
    let mut viewer = tls(client, invitation).await.unwrap();
    let authenticate = ViewerMessage::Authenticate {
        token,
        codecs,
        limits: VideoLimits::for_viewer(60.0),
        quality: QualityPreset::Balanced,
        capabilities,
    };
    wire::write_message(&mut viewer.stream, &authenticate, None)
        .await
        .unwrap();
    let answer = wire::read_header(&mut viewer.stream, Duration::from_secs(5))
        .await
        .unwrap()
        .unwrap();
    (viewer, answer)
}

/// The next message of a kind other than probe or stats, past a video
/// unit's payload record; `None` at a clean close.
async fn next(viewer: &mut Viewer) -> Option<HostMessage> {
    loop {
        match wire::read_header(&mut viewer.stream, Duration::from_secs(5))
            .await
            .unwrap()?
        {
            HostMessage::Probe { .. } | HostMessage::Stats { .. } => {}
            message @ HostMessage::Video { .. } => {
                wire::read_payload(&mut viewer.stream).await.unwrap();
                return Some(message);
            }
            message => return Some(message),
        }
    }
}

fn rejected(answer: &HostMessage) -> Option<RejectReason> {
    match answer {
        HostMessage::Rejected { reason } => Some(*reason),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_reports_denied_and_busy_before_close() {
    let relay = pb_mapper_testkit::Relay::start("desktop-handshake").await;
    let client = Client::from_credential(
        relay.addr().to_string(),
        pb_mapper_testkit::admin_credential(),
        true,
        None,
    );
    let backends = Arc::new(AtomicU32::new(0));
    let created = backends.clone();
    let host = Host::builder(options())
        .backend(move || {
            created.fetch_add(1, Ordering::Relaxed);
            Ok(Screen)
        })
        .start(&client)
        .await
        .unwrap();
    let invitation = host.invitation().clone();
    let token = invitation.token_text();
    let wrong = "A".repeat(token.len());

    // A wrong token is denied, then closed, before any capture starts.
    let (mut denied, answer) =
        authenticate(&client, &invitation, wrong.clone(), vec![VideoCodec::Jpeg]).await;
    assert_eq!(rejected(&answer), Some(RejectReason::Denied));
    assert!(next(&mut denied).await.is_none());
    assert_eq!(backends.load(Ordering::Relaxed), 0);

    // The first viewer holds the slot ...
    let (mut first, answer) =
        authenticate(&client, &invitation, token.clone(), vec![VideoCodec::Jpeg]).await;
    assert!(
        matches!(
            answer,
            HostMessage::Accepted {
                codec: VideoCodec::Jpeg,
                nominal_fps: 60,
                ..
            }
        ),
        "{answer:?}"
    );
    // ... so a second viewer with the right token is busy, a malformed
    // request is invalid, and a wrong token is still denied, never busy.
    let (_, answer) =
        authenticate(&client, &invitation, token.clone(), vec![VideoCodec::H264]).await;
    assert_eq!(rejected(&answer), Some(RejectReason::Busy));
    let (_, answer) = authenticate(&client, &invitation, token.clone(), vec![]).await;
    assert_eq!(rejected(&answer), Some(RejectReason::InvalidRequest));
    let (_, answer) = authenticate(&client, &invitation, wrong, vec![VideoCodec::Jpeg]).await;
    assert_eq!(rejected(&answer), Some(RejectReason::Denied));

    // The first viewer's session is unaffected.
    assert!(matches!(
        next(&mut first).await,
        Some(HostMessage::State(_))
    ));
    drop(first);
    host.stop().await.unwrap();
}

/// Poll `condition` for up to 5 s.
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_reports_the_viewer_and_end_session_keeps_hosting() {
    let relay = pb_mapper_testkit::Relay::start("desktop-viewer").await;
    let client = Client::from_credential(
        relay.addr().to_string(),
        pb_mapper_testkit::admin_credential(),
        true,
        None,
    );
    let mut options = options();
    options.allow_control = true;
    let host = Host::builder(options)
        .backend(|| Ok(Screen))
        .start(&client)
        .await
        .unwrap();
    let status = host.status();
    assert_eq!(status.state, HostState::Waiting);
    assert!(status.viewer.is_none() && status.control_allowed);
    // An embedder's backend that describes nothing can do nothing more.
    assert!(status.capabilities.is_empty());

    let invitation = host.invitation().clone();
    let token = invitation.token_text();
    let (mut viewer, answer) =
        authenticate(&client, &invitation, token.clone(), vec![VideoCodec::Jpeg]).await;
    assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
    // The host publishes the session just after writing `Accepted`.
    until(|| host.status().viewer.is_some()).await;
    let watching = host.status().viewer.unwrap();
    assert_eq!(
        (
            watching.path,
            watching.controlling,
            watching.paused,
            watching.quality
        ),
        (HostPath::Relay, false, false, QualityPreset::Balanced)
    );
    assert!(watching.started_unix_ms > 0);

    // The viewer hides and picks another preset; the host shows both.
    for message in [
        ViewerMessage::Pause {},
        ViewerMessage::Quality {
            preset: QualityPreset::Smooth,
        },
    ] {
        wire::write_message(&mut viewer.stream, &message, None)
            .await
            .unwrap();
    }
    until(|| {
        host.status()
            .viewer
            .is_some_and(|viewer| viewer.paused && viewer.quality == QualityPreset::Smooth)
    })
    .await;
    assert_eq!(host.status().state, HostState::Paused);

    // Disconnect ends this session only: the viewer hears why ...
    host.end_session();
    loop {
        match next(&mut viewer).await {
            Some(HostMessage::Stopped { reason, .. }) => {
                assert_eq!(reason, StopReason::HostEnded);
                break;
            }
            Some(_) => {}
            None => panic!("closed without Stopped"),
        }
    }
    assert!(next(&mut viewer).await.is_none());
    until(|| host.status().viewer.is_none()).await;
    assert_eq!(host.status().state, HostState::Waiting);

    // ... and hosting continues with the same invitation.
    let (_again, answer) = authenticate(&client, &invitation, token, vec![VideoCodec::Jpeg]).await;
    assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
    host.stop().await.unwrap();
    assert_eq!(host.status().state, HostState::Stopped);
}

/// A screen whose backend encodes H.265 until `broken`, then fails as a
/// GPU's H.265 encoder that stopped encoding does. It keeps the bitrate
/// each H.265 picture was encoded at.
struct Hevc {
    broken: Arc<AtomicBool>,
    bitrates: Arc<Mutex<Vec<u32>>>,
}
impl crate::Backend for Hevc {
    fn display(&mut self) -> Result<crate::Display> {
        crate::Backend::display(&mut Screen)
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Ok(vec![1])
    }
    fn capture_video(
        &mut self,
        _: &crate::DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        rate: crate::VideoRate,
    ) -> Result<Option<crate::EncodedFrame>> {
        if codec == VideoCodec::H265 && self.broken.load(Ordering::SeqCst) {
            return Err(crate::BackendError::new(
                crate::FailureKind::EncoderFailed { codec },
                "the encoder stopped",
            )
            .into());
        }
        if codec == VideoCodec::H265 {
            self.bitrates.lock().push(rate.bitrate);
        }
        Ok(Some(crate::EncodedFrame {
            format: crate::VideoFormat {
                codec,
                width: 10,
                height: 10,
                keyframe,
                ..crate::VideoFormat::default()
            },
            bytes: vec![1],
        }))
    }
    fn input(&mut self, _: &crate::Input) -> Result<()> {
        Err(error("read-only test backend"))
    }
    fn release(&mut self) {}
    /// It can lock, says nothing of its OS, and answers H.265 from a cache,
    /// whether asked or not: the host keeps H.265 off after a failure anyway.
    fn describe(&mut self, _: bool) -> Result<crate::Description> {
        Ok(crate::Description {
            display: self.display()?,
            caps: BackendCaps {
                h265: true,
                lock: true,
                ..BackendCaps::default()
            },
        })
    }
}

/// A backend slower to describe itself than `Accepted` waits leaves its
/// session with what its launcher falls back to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_description_falls_back_to_the_launcher() {
    struct Late;
    impl crate::Backend for Late {
        fn display(&mut self) -> Result<crate::Display> {
            crate::Backend::display(&mut Screen)
        }
        fn input(&mut self, _: &crate::Input) -> Result<()> {
            Err(error("read-only test backend"))
        }
        fn release(&mut self) {}
        fn describe(&mut self, _: bool) -> Result<crate::Description> {
            std::thread::sleep(BACKEND_ANSWER_WAIT + Duration::from_millis(500));
            Ok(crate::Description {
                display: self.display()?,
                caps: BackendCaps::default(),
            })
        }
    }
    let relay = pb_mapper_testkit::Relay::start("desktop-late").await;
    let client = Client::from_credential(
        relay.addr().to_string(),
        pb_mapper_testkit::admin_credential(),
        true,
        None,
    );
    let fallback = BackendCaps {
        lock: true,
        os: Some(crate::HostOs::Windows),
        ..BackendCaps::default()
    };
    let host = Host::builder(options())
        .launch(Arc::new(Launcher::new(|| Ok(Late), fallback)))
        .start(&client)
        .await
        .unwrap();
    let invitation = host.invitation().clone();
    let (_viewer, answer) = authenticate(
        &client,
        &invitation,
        invitation.token_text(),
        vec![VideoCodec::Jpeg],
    )
    .await;
    let HostMessage::Accepted { capabilities, .. } = answer else {
        panic!("{answer:?}");
    };
    assert_eq!(capabilities, ["lock", "os_windows"]);
    host.stop().await.unwrap();
}

/// The only test that breaks an H.265 encoder: that turns H.265 off for
/// the whole test process.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h265_needs_both_ends_and_ends_with_its_encoder() {
    let relay = pb_mapper_testkit::Relay::start("desktop-h265").await;
    let client = Client::from_credential(
        relay.addr().to_string(),
        pb_mapper_testkit::admin_credential(),
        true,
        None,
    );
    let broken = Arc::new(AtomicBool::new(false));
    let bitrates = Arc::new(Mutex::new(Vec::new()));
    let (encoder, encoded) = (broken.clone(), bitrates.clone());
    let host = Host::builder(options())
        .backend(move || {
            Ok(Hevc {
                broken: encoder.clone(),
                bitrates: encoded.clone(),
            })
        })
        .start(&client)
        .await
        .unwrap();
    let invitation = host.invitation().clone();
    let token = invitation.token_text();
    let h265 = || vec![CAPABILITY_H265.to_string()];
    let codec = |answer: &HostMessage| match answer {
        HostMessage::Accepted {
            codec,
            capabilities,
            ..
        } => {
            // What this session's backend described, nothing more.
            assert_eq!(capabilities, &[crate::protocol::CAPABILITY_LOCK]);
            Some(*codec)
        }
        _ => None,
    };

    // A viewer that does not decode H.265 gets what every v2 host gives.
    let (viewer, answer) =
        authenticate(&client, &invitation, token.clone(), vec![VideoCodec::H264]).await;
    assert_eq!(codec(&answer), Some(VideoCodec::H264));
    drop(viewer);
    until(|| host.status().viewer.is_none()).await;
    // H.265 is a capability, never a codec offered.
    let (_, answer) =
        authenticate(&client, &invitation, token.clone(), vec![VideoCodec::H265]).await;
    assert_eq!(rejected(&answer), Some(RejectReason::InvalidRequest));

    // Both ends can: H.265, and its video.
    let (mut viewer, answer) = authenticate_with(
        &client,
        &invitation,
        token.clone(),
        vec![VideoCodec::H264],
        h265(),
    )
    .await;
    assert_eq!(codec(&answer), Some(VideoCodec::H265));
    // The encoder fails once video flows: the session ends, and says so.
    let mut videos = 0;
    let reason = loop {
        match next(&mut viewer).await {
            Some(HostMessage::Video { seq, .. }) => {
                videos += 1;
                if videos == 3 {
                    broken.store(true, Ordering::SeqCst);
                }
                let receipt = ViewerMessage::Received { seq };
                wire::write_message(&mut viewer.stream, &receipt, None)
                    .await
                    .unwrap();
            }
            Some(HostMessage::Stopped { reason, .. }) => break reason,
            Some(_) => {}
            None => panic!("closed without Stopped"),
        }
    };
    assert_eq!(reason, StopReason::CaptureFailed);
    // Its first IDR was budgeted for the 10×10 display the backend
    // reported, not for the viewer's 1080p bounds: the first unit's size
    // changed nothing.
    let encoded = bitrates.lock().clone();
    assert!(
        encoded.len() >= 3 && encoded.iter().all(|bitrate| *bitrate == encoded[0]),
        "{encoded:?}"
    );
    drop(viewer);
    until(|| host.status().viewer.is_none()).await;
    // The reconnect gets H.264, from this process on.
    let (_viewer, answer) =
        authenticate_with(&client, &invitation, token, vec![VideoCodec::H264], h265()).await;
    assert_eq!(codec(&answer), Some(VideoCodec::H264));
    host.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remembered_invitation_survives_restart_but_not_revocation() {
    let relay = pb_mapper_testkit::Relay::start("desktop-trust").await;
    let client = Client::from_credential(
        relay.addr().to_string(),
        pb_mapper_testkit::admin_credential(),
        true,
        None,
    );
    let identity = crate::HostIdentity::generate().unwrap();
    let saved = identity.encode_secret().unwrap();
    let start = |identity| {
        Host::builder(options())
            .identity(identity)
            .backend(|| Ok(Screen))
            .start(&client)
    };
    let host = start(identity).await.unwrap();
    // What the viewer keeps is the pbd1 text.
    let invitation = Invitation::decode(&host.invitation().encode()).unwrap();
    host.stop().await.unwrap();
    let resumed = start(crate::HostIdentity::decode_secret(&saved).unwrap())
        .await
        .unwrap();
    let viewer = async |control_allowed: bool| {
        let (mut viewer, answer) = authenticate(
            &client,
            &invitation,
            invitation.token_text(),
            vec![VideoCodec::Jpeg],
        )
        .await;
        assert!(matches!(answer, HostMessage::Accepted { .. }), "{answer:?}");
        let Some(HostMessage::State(state)) = next(&mut viewer).await else {
            panic!("no state");
        };
        assert_eq!(state.control_allowed, control_allowed);
        viewer
    };
    let mut watching = viewer(false).await;

    // Another display keeps the identity, the backend and the live
    // control policy; the viewer hears why it may reconnect.
    resumed.set_control_allowed(true);
    let switched = resumed
        .switch_display(crate::DisplaySelection::Id(2))
        .await
        .unwrap();
    loop {
        match next(&mut watching).await {
            Some(HostMessage::Stopped { reason, .. }) => {
                assert_eq!(reason, StopReason::Restarting);
                break;
            }
            Some(_) => {}
            None => panic!("closed without Stopped"),
        }
    }
    assert_eq!(switched.invitation().encode(), invitation.encode());
    assert_eq!(switched.options().display, crate::DisplaySelection::Id(2));
    drop(viewer(true).await);
    switched.stop().await.unwrap();

    // A new identity revokes the remembered invitation: the pin fails.
    let replacement = start(crate::HostIdentity::generate().unwrap())
        .await
        .unwrap();
    let failure = tls(&client, &invitation).await.err().unwrap();
    assert_eq!(
        security::handshake_failure(&failure),
        Some(security::HandshakeFailure::IdentityChanged)
    );
    replacement.stop().await.unwrap();
}
