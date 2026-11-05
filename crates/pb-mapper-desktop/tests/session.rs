#![allow(clippy::unwrap_used)]
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_desktop::{
    Backend, Display, Frame, Host, HostOptions, Input, InputEvent, Key, Result, Viewer,
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
        let mut bad = host.invitation().clone();
        bad.token = "x".repeat(43);
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
        let viewed = frame(&mut frames, |f| f.status.state == "streaming").await;
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
        host.stop().await.unwrap();
        assert_eq!(observed.dropped.load(Ordering::Relaxed), 1);
    }
}
