//! Status as a stream: `Host::watch` and `Viewer::watch` publish a status
//! when it changes, and end with the host or the viewer.
use crate::{Host, HostState, HostStatus, Viewer, ViewerState, ViewerStatus};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::watch;

impl Host {
    /// This host's status now, then each time it changes, sampled every
    /// `every`; a viewer's `since_ms` alone is no change. The stream ends,
    /// `changed()` failing, once the host has stopped: its last status says
    /// `Stopped`, unless the host was dropped instead. Dropping every
    /// receiver ends its task. Needs a Tokio runtime.
    pub fn watch(&self, every: Duration) -> watch::Receiver<HostStatus> {
        let handle = self.status_handle();
        let stopping = handle.clone();
        spawn(
            every,
            move || handle.status(),
            move || stopping.stopped(),
            |status| status.state == HostState::Stopped,
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
    /// `presented`, `last_video_age_ms`, `fresh` and a notice's age; read
    /// them with [`Self::status`], or [`Self::view`] per frame. The stream
    /// ends, `changed()` failing, after the stopped status. Dropping every
    /// receiver ends its task. Needs a Tokio runtime.
    pub fn watch(&self, every: Duration) -> watch::Receiver<ViewerStatus> {
        let handle = self.status_handle();
        let stopping = handle.clone();
        spawn(
            every,
            move || handle.status(),
            move || stopping.stopped(),
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

/// How long a stopping host may take to say so; a dropped one never does.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// A receiver of `status()`, sent again whenever its projection (its JSON
/// value less what `project` removes) differs from the last one sent. Once
/// `stopping`, the task ends at the first status that is `stopped`, or after
/// [`STOP_GRACE`], having sent it.
fn spawn<S: Serialize + Send + Sync + 'static>(
    every: Duration,
    status: impl Fn() -> S + Send + 'static,
    stopping: impl Fn() -> bool + Send + 'static,
    stopped: fn(&S) -> bool,
    project: fn(&mut Value),
) -> watch::Receiver<S> {
    let projection = move |status: &S| {
        let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
        project(&mut value);
        value
    };
    let first = status();
    let mut last = projection(&first);
    let (sender, receiver) = watch::channel(first);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every.max(Duration::from_millis(1)));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await;
        let mut since = None;
        loop {
            tokio::select! {
                _ = ticks.tick() => {}
                () = sender.closed() => return,
            }
            // Before the status, so a stop seen here is in what is read next.
            if stopping() {
                since.get_or_insert_with(Instant::now);
            }
            let status = status();
            let end = since
                .is_some_and(|since: Instant| stopped(&status) || since.elapsed() >= STOP_GRACE);
            let value = projection(&status);
            if value != last {
                last = value;
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
        let host = Host::builder(options)
            .backend(|| Ok(Still))
            .start(&client)
            .await
            .unwrap();
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
    }
}
