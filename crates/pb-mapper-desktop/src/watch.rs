//! Status as a stream: `Host::watch` and `Viewer::watch` publish a status
//! when it changes, and end with the host or the viewer.
use crate::{Host, HostStatus, Viewer, ViewerState, ViewerStatus};
use serde::Serialize;
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio::sync::watch;

impl Host {
    /// This host's status now, then each time it changes, sampled every
    /// `every`; a viewer's `since_ms` alone is no change. The stream ends,
    /// `changed()` failing, once a stop has finished, its last status
    /// `Stopped`, or once the host was dropped. It never keeps the host or
    /// its registration alive, and dropping every receiver ends its task.
    /// Needs a Tokio runtime.
    pub fn watch(&self, every: Duration) -> watch::Receiver<HostStatus> {
        let handle = self.status_handle();
        let done = handle.stopped();
        spawn(
            every,
            self.status(),
            move || handle.status(),
            done,
            |_| false,
            |value| {
                if let Some(viewer) = value.get_mut("viewer").and_then(Value::as_object_mut) {
                    viewer.remove("since_ms");
                }
            },
        )
    }
}

impl Viewer {
    /// This viewer's status now, then each time it changes, sampled every
    /// `every`. What moves with every frame is no change: `stats`,
    /// `presented`, `last_video_age_ms`, `fresh` and a notice's age (each new
    /// notice has a new `seq`); read them with [`Self::status`], or
    /// [`Self::view`] per frame. The stream ends, `changed()` failing, after
    /// the stopped status. Dropping every receiver ends its task. Needs a
    /// Tokio runtime.
    pub fn watch(&self, every: Duration) -> watch::Receiver<ViewerStatus> {
        let handle = self.status_handle();
        spawn(
            every,
            self.status(),
            move || Some(handle.status()),
            std::future::pending(),
            |status| status.state == ViewerState::Stopped,
            |value| {
                if let Some(status) = value.as_object_mut() {
                    for field in ["stats", "presented", "last_video_age_ms", "fresh"] {
                        status.remove(field);
                    }
                    if let Some(notice) = status.get_mut("notice").and_then(Value::as_object_mut) {
                        notice.remove("age_ms");
                    }
                }
            },
        )
    }
}

/// A receiver of `first`, then of `status()` whenever its projection (its
/// JSON value less what `project` removes) differs from the last one sent.
/// The task ends after sending a status that is `last`, after the status read
/// once `done` resolved, or when `status()` has nothing left to read.
fn spawn<S: Serialize + Send + Sync + 'static>(
    every: Duration,
    first: S,
    status: impl Fn() -> Option<S> + Send + 'static,
    done: impl Future<Output = ()> + Send + 'static,
    last: fn(&S) -> bool,
    project: fn(&mut Value),
) -> watch::Receiver<S> {
    let projection = move |status: &S| {
        let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
        project(&mut value);
        value
    };
    let mut sent = projection(&first);
    let (sender, receiver) = watch::channel(first);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every.max(Duration::from_millis(1)));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await;
        tokio::pin!(done);
        loop {
            let finished = tokio::select! {
                _ = ticks.tick() => false,
                () = &mut done => true,
                () = sender.closed() => return,
            };
            let Some(status) = status() else {
                return;
            };
            let end = finished || last(&status);
            let value = projection(&status);
            if value != sent {
                sent = value;
                if sender.send(status).is_err() {
                    return;
                }
            }
            if end {
                return;
            }
        }
    });
    receiver
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::{
        Backend, Display, Host, HostOptions, HostState, Input, Pull, Result, VideoCodec, Viewer,
        ViewerConfig, ViewerState,
    };
    use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
    use std::time::Duration;

    struct Still;
    impl Backend for Still {
        fn display(&mut self) -> Result<Display> {
            Ok(Display {
                id: 1,
                name: "watched".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1; 64])
        }
        fn input(&mut self, _: &Input) -> Result<()> {
            Ok(())
        }
        fn release(&mut self) {}
    }
    /// A teardown slower than any grace a watch could allow a stop.
    impl Drop for Still {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_millis(2500));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn watch_sends_changes_not_frames_and_ends_stopped() {
        let relay = pb_mapper_testkit::Relay::start("desktop-watch").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let mut options = HostOptions::new("desktop:watch");
        options.allow_direct = false;
        let start = || {
            Host::builder(options.clone())
                .backend(|| Ok(Still))
                .start(&client)
        };
        let host = start().await.unwrap();
        let viewer = Viewer::connect(
            &client,
            host.invitation(),
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
            ViewerConfig::new(vec![VideoCodec::Jpeg]),
        )
        .await
        .unwrap();
        // A renderer that shows every picture, so frames keep moving.
        let consumer = viewer.video_sink().attach().unwrap();
        let renderer = std::thread::spawn(move || {
            loop {
                match consumer.next(Duration::from_millis(50), 1 << 20) {
                    Pull::Unit(unit) => {
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::Closed => break,
                    _ => {}
                }
            }
        });
        let every = Duration::from_millis(20);
        let mut hosting = host.watch(every);
        let mut viewing = viewer.watch(every);
        tokio::time::timeout(Duration::from_secs(10), async {
            while viewing.borrow_and_update().state != ViewerState::Streaming {
                viewing.changed().await.unwrap();
            }
            while viewer.status().presented.is_none() {
                tokio::time::sleep(every).await;
            }
            viewing.borrow_and_update();
            assert!(
                tokio::time::timeout(Duration::from_millis(500), viewing.changed())
                    .await
                    .is_err(),
                "a status that only frames moved was sent"
            );
            host.stop().await.unwrap();
            while viewing.changed().await.is_ok() {}
            assert_eq!(viewing.borrow().state, ViewerState::Stopped);
            while hosting.changed().await.is_ok() {}
            assert_eq!(hosting.borrow().state, HostState::Stopped);
        })
        .await
        .unwrap();
        viewer.stop().await.unwrap();
        renderer.join().unwrap();

        // However slowly a dropped host is watched, its stream ends and its
        // service leaves the relay.
        let host = start().await.unwrap();
        let mut slow = host.watch(Duration::from_secs(3600));
        drop(host);
        tokio::time::timeout(Duration::from_secs(5), async {
            while slow.changed().await.is_ok() {}
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while client
                .list_keys()
                .await
                .unwrap()
                .iter()
                .any(|key| key.contains("desktop:watch"))
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    }
}
