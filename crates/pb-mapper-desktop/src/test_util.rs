//! What this crate's own tests share.
use crate::{Host, Pull, VideoCodec, Viewer, ViewerConfig, ViewerStatus};
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Wait until `condition` holds, looking every 10 ms; fail after 10 s.
pub(crate) async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// An H.264 viewer over the relay whose renderer, on its own thread,
/// presents every unit at once.
pub(crate) struct Presenting {
    pub(crate) viewer: Viewer,
    stop: Arc<AtomicBool>,
    renderer: std::thread::JoinHandle<()>,
}
impl Presenting {
    pub(crate) async fn connect(client: &Client, host: &Host) -> Self {
        let options = ConnectOptions {
            path: PathPolicy::RelayOnly,
            ..Default::default()
        };
        let config = ViewerConfig::new(vec![VideoCodec::H264]);
        let viewer = Viewer::connect(client, host.invitation(), options, config)
            .await
            .unwrap();
        let consumer = viewer.video_sink().attach().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let renderer = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                match consumer.next(Duration::from_millis(20), 4 << 20) {
                    Pull::Unit(unit) => {
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::Closed => break,
                    Pull::Timeout | Pull::Paused | Pull::NeedCapacity(_) => {}
                }
            }
        });
        Self {
            viewer,
            stop,
            renderer,
        }
    }

    /// The viewer's status once `condition` holds.
    pub(crate) async fn until(&self, condition: impl Fn(&ViewerStatus) -> bool) -> ViewerStatus {
        until(|| condition(&self.viewer.status())).await;
        self.viewer.status()
    }

    /// Ask for control once a frame is fresh; the status once it is granted
    /// over a fresh frame.
    pub(crate) async fn control(&self) -> ViewerStatus {
        self.until(|status| status.fresh).await;
        self.viewer.request_control(true).unwrap();
        self.until(|status| status.controlling && status.fresh)
            .await
    }

    pub(crate) async fn stop(self) {
        self.viewer.stop().await.unwrap();
        self.stop.store(true, Ordering::Release);
        self.renderer.join().unwrap();
    }
}
