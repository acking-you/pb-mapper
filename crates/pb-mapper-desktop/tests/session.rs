#![allow(clippy::unwrap_used)]
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_desktop::{
    Backend, ConnectError, CursorPosition, DesktopContext, Display, EncodedFrame, Host,
    HostOptions, HostState, Input, InputError, Invitation, Key, Pull, QualityPreset, Result,
    VideoCodec, VideoFormat, VideoLimits, VideoRate, Viewer, ViewerConfig, ViewerState,
    ViewerStatus,
};
use pb_mapper_server::DirectServerConfig;
use pb_mapper_testkit::{Relay, admin_credential};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Observation {
    captures: AtomicU32,
    inputs: AtomicU32,
    releases: AtomicU32,
    dropped: AtomicU32,
    changed: Notify,
}
struct Desktop(Arc<Observation>);
impl Backend for Desktop {
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "Test display".into(),
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        self.0.captures.fetch_add(1, Ordering::Relaxed);
        Ok(vec![10, 20, 30, 40])
    }
    fn input(&mut self, _: &Input) -> Result<()> {
        self.0.inputs.fetch_add(1, Ordering::Relaxed);
        self.0.changed.notify_one();
        Ok(())
    }
    fn release(&mut self) {
        self.0.releases.fetch_add(1, Ordering::Relaxed);
        self.0.changed.notify_one();
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// The same host and pin with another bearer token, as a well-formed `pbd1`.
fn forged(invitation: &Invitation) -> Invitation {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let text = invitation.encode();
    let mut payload = URL_SAFE_NO_PAD.decode(&text["pbd1.".len()..]).unwrap();
    let check = payload.len() - 4;
    payload[check - 32..check].fill(b'x');
    let digest = ring::digest::digest(&ring::digest::SHA256, &payload[..check]);
    payload[check..].copy_from_slice(&digest.as_ref()[..4]);
    Invitation::decode(&format!("pbd1.{}", URL_SAFE_NO_PAD.encode(payload))).unwrap()
}

fn config(codec: VideoCodec) -> ViewerConfig {
    ViewerConfig {
        codecs: vec![codec],
        limits: VideoLimits::for_viewer(60.0),
        quality: QualityPreset::Balanced,
        progress: None,
    }
}

/// A renderer on its own thread, as a native consumer runs: it pulls every
/// unit and, while `presenting`, presents it at once.
struct Renderer {
    presenting: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Renderer {
    fn start(viewer: &Viewer, presenting: bool) -> Self {
        let consumer = viewer.video_sink().attach().unwrap();
        let presenting = Arc::new(AtomicBool::new(presenting));
        let stop = Arc::new(AtomicBool::new(false));
        let (present, done) = (presenting.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                match consumer.next(Duration::from_millis(50), 4 << 20) {
                    Pull::Unit(unit) if present.load(Ordering::Acquire) => {
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::Closed => break,
                    _ => {}
                }
            }
        });
        Self {
            presenting,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

async fn wait(viewer: &Viewer, condition: impl Fn(&ViewerStatus) -> bool) -> ViewerStatus {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = viewer.status();
            if condition(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

const SHIFT: [Input; 1] = [Input::Key {
    key: Key::Shift,
    down: true,
}];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the v2 host session; runs after the G1 merges"]
async fn encrypted_desktop_direct_and_relay_enforce_grants_and_owned_teardown() {
    let mut relay = Relay::start_with_direct(
        "desktop-session",
        Some(DirectServerConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            advertise: "127.0.0.1:0".parse().unwrap(),
        }),
    )
    .await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    for path in [PathPolicy::RelayOnly, PathPolicy::Auto] {
        let options = || ConnectOptions {
            path,
            ..Default::default()
        };
        let observed = Arc::new(Observation::default());
        let backend = observed.clone();
        let host = Host::start_with_backend(
            &client,
            HostOptions {
                service: "desktop:test".into(),
                display_id: 1,
                allow_control: true,
                allow_direct: true,
            },
            Arc::new(move || Ok(Box::new(Desktop(backend.clone())))),
        )
        .await
        .unwrap();
        // A valid relay credential does not grant desktop access.
        let intruder = Viewer::connect(
            &client,
            &forged(host.invitation()),
            options(),
            config(VideoCodec::Jpeg),
        )
        .await;
        assert!(matches!(intruder, Err(ConnectError::Denied)));
        assert_eq!(observed.captures.load(Ordering::Relaxed), 0);

        let viewer = Viewer::connect(
            &client,
            host.invitation(),
            options(),
            config(VideoCodec::Jpeg),
        )
        .await
        .unwrap();
        let renderer = Renderer::start(&viewer, true);
        let viewed = wait(&viewer, |status| status.fresh).await;
        assert_eq!(
            viewed.stats.path,
            if path == PathPolicy::Auto {
                "direct"
            } else {
                "relay"
            }
        );
        assert!(matches!(
            viewer.input(viewed.generation, &SHIFT),
            Err(InputError::NotControlling)
        ));
        viewer.request_control(true).unwrap();
        let controlled = wait(&viewer, |status| status.controlling && status.fresh).await;
        viewer.input(controlled.generation, &SHIFT).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while observed.inputs.load(Ordering::Relaxed) == 0 {
                observed.changed.notified().await;
            }
        })
        .await
        .unwrap();
        let releases = observed.releases.load(Ordering::Relaxed);
        for _ in 0..1000 {
            viewer.release_input().unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while observed.releases.load(Ordering::Relaxed) <= releases {
                observed.changed.notified().await;
            }
        })
        .await
        .unwrap();
        host.set_control_allowed(false);
        let revoked = wait(&viewer, |status| {
            !status.controlling && !status.control_allowed
        })
        .await;
        assert!(viewer.input(revoked.generation, &SHIFT).is_err());
        assert_eq!(observed.inputs.load(Ordering::Relaxed), 1);
        assert!(observed.releases.load(Ordering::Relaxed) > 0);
        // Direct ownership survives coordinator loss; relay transport need not.
        if path == PathPolicy::Auto {
            relay.stop_server().await;
            let frame = revoked.presented.unwrap().frame_id;
            wait(&viewer, |status| {
                status
                    .presented
                    .is_some_and(|presented| presented.frame_id > frame)
            })
            .await;
        }
        viewer.stop().await.unwrap();
        drop(renderer);
        let stopped = host.status();
        assert_eq!(stopped.state, HostState::Waiting);
        assert!(stopped.message.is_empty(), "{}", stopped.message);
        host.stop().await.unwrap();
        assert_eq!(observed.dropped.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the v2 host session; runs after the G1 merges"]
async fn frozen_renderer_keeps_credit_and_cursor_but_never_input() {
    /// A static H.264 desktop with a moving pointer.
    struct Static {
        cursor: Arc<AtomicU32>,
        frames: u32,
    }
    impl Backend for Static {
        fn display(&mut self) -> Result<Display> {
            Ok(Display {
                id: 1,
                name: "static desktop".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            unreachable!("test uses H264")
        }
        fn capture_video(
            &mut self,
            _: &DesktopContext,
            _: VideoCodec,
            keyframe: bool,
            _: VideoRate,
        ) -> Result<Option<EncodedFrame>> {
            self.frames += 1;
            Ok(Some(EncodedFrame {
                format: VideoFormat {
                    codec: VideoCodec::H264,
                    width: 1920,
                    height: 1080,
                    keyframe: keyframe || self.frames == 1,
                    ..Default::default()
                },
                bytes: vec![1; 4096],
            }))
        }
        fn cursor(&mut self, _: &DesktopContext) -> Result<Option<CursorPosition>> {
            Ok(Some(CursorPosition {
                x: self.cursor.load(Ordering::Relaxed) as i32,
                y: 0,
                visible: true,
            }))
        }
        fn input(&mut self, _: &Input) -> Result<()> {
            Ok(())
        }
        fn release(&mut self) {}
    }
    let relay = Relay::start("desktop-frozen").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let cursor = Arc::new(AtomicU32::new(0));
    let backend = cursor.clone();
    let host = Host::start_with_backend(
        &client,
        HostOptions {
            service: "desktop:frozen".into(),
            display_id: 1,
            allow_control: true,
            allow_direct: false,
        },
        Arc::new(move || {
            Ok(Box::new(Static {
                cursor: backend.clone(),
                frames: 0,
            }))
        }),
    )
    .await
    .unwrap();
    let viewer = Viewer::connect(
        &client,
        host.invitation(),
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
        config(VideoCodec::H264),
    )
    .await
    .unwrap();
    let renderer = Renderer::start(&viewer, true);
    wait(&viewer, |status| status.fresh).await;
    viewer.request_control(true).unwrap();
    let controlled = wait(&viewer, |status| status.controlling && status.fresh).await;

    // The renderer freezes: it keeps pulling but never presents.
    renderer.presenting.store(false, Ordering::Release);
    let frozen = wait(&viewer, |status| !status.fresh).await;
    let received = frozen.stats.received_fps;
    let seq = viewer.view().cursor_seq;
    cursor.store(17, Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(3), async {
        while viewer.view().cursor_seq == seq || viewer.view().cursor_x != 17 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        viewer.input(controlled.generation, &SHIFT),
        Err(InputError::NotFresh)
    ));
    tokio::time::sleep(Duration::from_secs(5)).await;
    let status = viewer.status();
    assert_eq!(status.state, ViewerState::Streaming, "{}", status.message);
    assert!(status.last_video_age_ms.is_some_and(|age| age < 1000));
    assert!(
        status.stats.received_fps > 0.0,
        "received {received} fps before"
    );

    // One presented frame restores input.
    renderer.presenting.store(true, Ordering::Release);
    let fresh = wait(&viewer, |status| status.fresh && status.controlling).await;
    viewer.input(fresh.generation, &SHIFT).unwrap();
    drop(renderer);
    viewer.stop().await.unwrap();
    host.stop().await.unwrap();
}
