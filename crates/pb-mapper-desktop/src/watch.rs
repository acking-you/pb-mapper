//! Status as a stream: `Host::watch` and `Viewer::watch` publish a status
//! when it changes, and end with the host or the viewer.
use crate::{Host, HostStatus, HostViewer, NoticeInfo, Viewer, ViewerState, ViewerStatus};
use std::{future::Future, time::Duration};
use tokio::sync::watch;

impl Host {
    /// This host's status now, then each time it changes, sampled every
    /// `every`; a viewer's `since_ms` alone is no change. The stream ends,
    /// `changed()` failing, once a stop has finished, its last status
    /// `Stopped` even when the host is dropped at once after, or once the
    /// host was dropped without a stop. It never keeps the host or
    /// its registration alive, and dropping every receiver ends its task.
    /// Needs a Tokio runtime.
    pub fn watch(&self, every: Duration) -> watch::Receiver<HostStatus> {
        let handle = self.status_handle();
        let done = handle.clone().stopped();
        spawn(
            every,
            self.status(),
            move || handle.status(),
            done,
            |_| false,
            same_host,
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
            std::future::pending::<Option<ViewerStatus>>(),
            |status| status.state == ViewerState::Stopped,
            same_viewer,
        )
    }
}

/// The same host status, a viewer's `since_ms` aside.
fn same_host(a: &HostStatus, b: &HostStatus) -> bool {
    let HostStatus {
        state,
        reason,
        message,
        control_allowed,
        viewer,
        display,
        desktop,
        capabilities,
        display_mode,
    } = a;
    *state == b.state
        && *reason == b.reason
        && *message == b.message
        && *control_allowed == b.control_allowed
        && match (viewer, &b.viewer) {
            (Some(a), Some(b)) => same_host_viewer(a, b),
            (a, b) => a.is_none() && b.is_none(),
        }
        && *display == b.display
        && *desktop == b.desktop
        && *capabilities == b.capabilities
        && *display_mode == b.display_mode
}

fn same_host_viewer(a: &HostViewer, b: &HostViewer) -> bool {
    let HostViewer {
        since_ms: _,
        started_unix_ms,
        path,
        controlling,
        paused,
        quality,
    } = a;
    *started_unix_ms == b.started_unix_ms
        && *path == b.path
        && *controlling == b.controlling
        && *paused == b.paused
        && *quality == b.quality
}

/// The same viewer status, what moves with every frame aside.
fn same_viewer(a: &ViewerStatus, b: &ViewerStatus) -> bool {
    let ViewerStatus {
        state,
        stage,
        reason,
        message,
        control_allowed,
        controlling,
        control_wanted,
        fresh: _,
        paused,
        generation,
        display,
        desktop,
        video,
        presented: _,
        last_video_age_ms: _,
        quality,
        modes,
        host_os,
        capabilities,
        notice,
        decoder,
        stats: _,
    } = a;
    *state == b.state
        && *stage == b.stage
        && *reason == b.reason
        && *message == b.message
        && *control_allowed == b.control_allowed
        && *controlling == b.controlling
        && *control_wanted == b.control_wanted
        && *paused == b.paused
        && *generation == b.generation
        && *display == b.display
        && *desktop == b.desktop
        && *video == b.video
        && *quality == b.quality
        && *modes == b.modes
        && *host_os == b.host_os
        && *capabilities == b.capabilities
        && match (notice, &b.notice) {
            (Some(a), Some(b)) => same_notice(a, b),
            (a, b) => a.is_none() && b.is_none(),
        }
        && *decoder == b.decoder
}

fn same_notice(a: &NoticeInfo, b: &NoticeInfo) -> bool {
    let NoticeInfo {
        code,
        message,
        seq,
        age_ms: _,
    } = a;
    *code == b.code && *message == b.message && *seq == b.seq
}

/// A receiver of `first`, then of `status()` whenever it is not the `same`
/// as the last one sent. The task ends after sending a status that is
/// `last`; once `done` resolves, after sending the status it gives, or else
/// the one `status()` reads then; or when `status()` has nothing left to
/// read.
fn spawn<S: Send + Sync + 'static>(
    every: Duration,
    first: S,
    status: impl Fn() -> Option<S> + Send + 'static,
    done: impl Future<Output = Option<S>> + Send + 'static,
    last: fn(&S) -> bool,
    same: fn(&S, &S) -> bool,
) -> watch::Receiver<S> {
    let (sender, receiver) = watch::channel(first);
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every.max(Duration::from_millis(1)));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await;
        tokio::pin!(done);
        loop {
            let (finished, status) = tokio::select! {
                biased;
                () = sender.closed() => return,
                final_status = &mut done => (true, final_status.or_else(&status)),
                _ = ticks.tick() => (false, status()),
            };
            let Some(status) = status else {
                return;
            };
            let end = finished || last(&status);
            // Only this task sends, so the channel holds the last status sent.
            let unchanged = same(&sender.borrow(), &status);
            if !unchanged && sender.send(status).is_err() {
                return;
            }
            if end {
                return;
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::{same_host, same_viewer};
    use crate::{
        Backend, DecoderInfo, DesktopKind, Display, Host, HostOptions, HostState, HostStatus,
        Input, NoticeCode, NoticeInfo, PresentedInfo, Pull, QualityPreset, Result, VideoCodec,
        Viewer, ViewerConfig, ViewerState, ViewerStats, ViewerStatus,
    };
    use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
    use std::time::Duration;

    /// What moves with every frame or with the clock is no change; a new
    /// notice, or anything else about the session, is.
    #[test]
    fn what_moves_with_frames_or_the_clock_is_no_change() {
        let viewer = ViewerStatus {
            state: ViewerState::Streaming,
            stage: None,
            reason: None,
            message: String::new(),
            control_allowed: true,
            controlling: false,
            control_wanted: None,
            fresh: false,
            paused: false,
            generation: 1,
            display: None,
            desktop: DesktopKind::Ordinary,
            video: None,
            presented: None,
            last_video_age_ms: None,
            quality: QualityPreset::Balanced,
            modes: None,
            host_os: None,
            capabilities: Vec::new(),
            notice: Some(NoticeInfo {
                code: NoticeCode::Locked,
                message: String::new(),
                seq: 1,
                age_ms: 0,
            }),
            decoder: DecoderInfo::default(),
            stats: ViewerStats::default(),
        };
        let mut framed = viewer.clone();
        framed.fresh = true;
        framed.presented = Some(PresentedInfo {
            generation: 1,
            frame_id: 2,
            age_ms: 3,
        });
        framed.last_video_age_ms = Some(4);
        framed.stats.presented_fps = 60.0;
        framed.notice.as_mut().unwrap().age_ms = 5;
        assert!(same_viewer(&viewer, &framed));
        framed.notice.as_mut().unwrap().seq = 2;
        assert!(!same_viewer(&viewer, &framed));

        let host: HostStatus = serde_json::from_value(serde_json::json!({
            "state": "streaming",
            "message": "",
            "control_allowed": true,
            "viewer": {
                "since_ms": 0,
                "started_unix_ms": 1,
                "path": "relay",
                "controlling": false,
                "paused": false,
                "quality": "balanced",
            },
            "desktop": "ordinary",
            "capabilities": [],
        }))
        .unwrap();
        let mut later = host.clone();
        later.viewer.as_mut().unwrap().since_ms = 1000;
        assert!(same_host(&host, &later));
        later.viewer.as_mut().unwrap().paused = true;
        assert!(!same_host(&host, &later));
    }

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

    /// A host dropped as soon as its stop returned, here before the watch
    /// task can run at all, still ends its stream on `Stopped`.
    #[tokio::test(flavor = "current_thread")]
    async fn a_host_dropped_after_its_stop_ends_its_watch_stopped() {
        let relay = pb_mapper_testkit::Relay::start("desktop-watch-stop").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let mut options = HostOptions::new("desktop:watch-stop");
        options.allow_direct = false;
        let host = Host::builder(options)
            .backend(|| Ok(Still))
            .start(&client)
            .await
            .unwrap();
        let mut hosting = host.watch(Duration::from_secs(3600));
        host.stop().await.unwrap();
        drop(host);
        tokio::time::timeout(Duration::from_secs(5), async {
            while hosting.changed().await.is_ok() {}
        })
        .await
        .unwrap();
        assert_eq!(hosting.borrow().state, HostState::Stopped);
    }
}
