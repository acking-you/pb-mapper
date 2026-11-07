#![allow(clippy::unwrap_used)]
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_desktop::{
    Backend, Display, Frame, Host, HostOptions, HostState, Input, InputEvent, Invitation, Key,
    Result, Viewer,
};
use pb_mapper_server::DirectServerConfig;
use pb_mapper_testkit::{Relay, admin_credential};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, watch};

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

async fn frame(
    rx: &mut watch::Receiver<Option<Frame>>,
    predicate: impl Fn(&Frame) -> bool,
) -> Frame {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(frame) = rx.borrow_and_update().clone().filter(&predicate) {
                return frame;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "v1 pipeline; rewritten for v2 after the G1 merges"]
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
        let bad = forged(host.invitation());
        let intruder = Viewer::connect(
            &client,
            bad,
            ConnectOptions {
                path,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), intruder.frames().changed())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(observed.captures.load(Ordering::Relaxed), 0);
        intruder.stop().await.unwrap();

        let viewer = Viewer::connect(
            &client,
            host.invitation().clone(),
            ConnectOptions {
                path,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut frames = viewer.frames();
        let viewed = frame(&mut frames, |f| !f.bytes.is_empty()).await;
        assert_eq!(&*viewed.bytes, &[10, 20, 30, 40]);
        assert_eq!(
            viewed.status.path,
            if path == PathPolicy::Auto {
                "direct"
            } else {
                "relay"
            }
        );
        let event = |f: &Frame, sequence| InputEvent {
            generation: f.status.generation,
            frame_id: f.status.frame_id,
            sequence,
            input: Input::Key {
                key: Key::Shift,
                down: true,
            },
        };
        assert!(viewer.input(event(&viewed, 1)).is_err());
        viewer.request_control(true).unwrap();
        let controlled = frame(&mut frames, |f| f.status.controlling).await;
        assert!(
            viewer.input(event(&viewed, 2)).is_err(),
            "old geometry is invalid after control admission"
        );
        viewer.input(event(&controlled, 3)).unwrap();
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
        frame(&mut frames, |f| {
            f.status.frame_id > controlled.status.frame_id
        })
        .await;
        assert_eq!(viewer.status().state, "streaming");
        viewer.input(event(&controlled, 3)).unwrap(); // host rejects duplicate
        host.set_control_allowed(false);
        let revoked = frame(&mut frames, |f| {
            !f.status.controlling && !f.status.control_allowed
        })
        .await;
        assert!(viewer.input(event(&revoked, 4)).is_err());
        assert_eq!(observed.inputs.load(Ordering::Relaxed), 1);
        assert!(observed.releases.load(Ordering::Relaxed) > 0);
        // Direct ownership survives coordinator loss; relay transport need not.
        if path == PathPolicy::Auto {
            relay.stop_server().await;
            let id = revoked.status.frame_id;
            frame(&mut frames, |f| f.status.frame_id > id).await;
        }
        viewer.stop().await.unwrap();
        let stopped = host.status();
        assert_eq!(stopped.state, HostState::Waiting);
        assert!(stopped.message.is_empty(), "{}", stopped.message);
        host.stop().await.unwrap();
        assert_eq!(observed.dropped.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "v1 pipeline; rewritten for v2 after the G1 merges"]
async fn cursor_crosses_a_full_video_window_without_refreshing_the_picture() {
    struct Snapshot {
        cursor: Arc<AtomicU32>,
        captured: bool,
    }
    impl Backend for Snapshot {
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
            _: &pb_mapper_desktop::DesktopContext,
            _: pb_mapper_desktop::VideoCodec,
            _: bool,
            _: pb_mapper_desktop::VideoRate,
        ) -> Result<Option<pb_mapper_desktop::EncodedFrame>> {
            if std::mem::replace(&mut self.captured, true) {
                return Ok(None);
            }
            Ok(Some(pb_mapper_desktop::EncodedFrame {
                format: pb_mapper_desktop::VideoFormat {
                    codec: pb_mapper_desktop::VideoCodec::H264,
                    width: 1920,
                    height: 1080,
                    keyframe: true,
                    ..Default::default()
                },
                // The single IDR exceeds initial credit, pausing video until
                // a real presentation. This test deliberately never ACKs it.
                bytes: vec![1; 128 * 1024],
            }))
        }
        fn cursor(
            &mut self,
            _: &pb_mapper_desktop::DesktopContext,
        ) -> Result<Option<pb_mapper_desktop::CursorPosition>> {
            Ok(Some(pb_mapper_desktop::CursorPosition {
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
    async fn next(viewer: &Viewer, changed: &mut watch::Receiver<Option<Frame>>) -> Frame {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(frame) = viewer.next_frame() {
                    return frame;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    }
    let relay = Relay::start("desktop-cursor").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let cursor = Arc::new(AtomicU32::new(0));
    let backend = cursor.clone();
    let host = Host::start_with_backend(
        &client,
        HostOptions {
            service: "desktop:cursor".into(),
            display_id: 1,
            allow_control: false,
            allow_direct: false,
        },
        Arc::new(move || {
            Ok(Box::new(Snapshot {
                cursor: backend.clone(),
                captured: false,
            }))
        }),
    )
    .await
    .unwrap();
    let viewer = Viewer::connect_with_presentation_feedback(
        &client,
        host.invitation().clone(),
        ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        },
        vec![pb_mapper_desktop::VideoCodec::H264],
        pb_mapper_desktop::VideoPreferences::default(),
    )
    .await
    .unwrap();
    let mut changed = viewer.frames();
    let initial = loop {
        let frame = next(&viewer, &mut changed).await;
        if !frame.bytes.is_empty() {
            break frame;
        }
    };
    cursor.store(17, Ordering::Relaxed);
    let moved = loop {
        let frame = next(&viewer, &mut changed).await;
        if frame.status.cursor.is_some_and(|cursor| cursor.x == 17) {
            break frame;
        }
    };
    assert!(moved.bytes.is_empty());
    assert_eq!(moved.status.frame_id, initial.status.frame_id);
    assert_eq!(moved.status.generation, initial.status.generation);
    assert_eq!(moved.status.frames, initial.status.frames);
    tokio::time::timeout(Duration::from_secs(3), async {
        while viewer.status().state != "stale" {
            let update = next(&viewer, &mut changed).await;
            assert!(update.bytes.is_empty());
        }
    })
    .await
    .unwrap();
    cursor.store(29, Ordering::Relaxed);
    loop {
        let update = next(&viewer, &mut changed).await;
        assert!(update.bytes.is_empty());
        if update.status.cursor.is_some_and(|cursor| cursor.x == 29) {
            break;
        }
    }
    assert_eq!(viewer.status().state, "stale");
    assert_eq!(viewer.status().frame_id, initial.status.frame_id);
    viewer.stop().await.unwrap();
    host.stop().await.unwrap();
}
