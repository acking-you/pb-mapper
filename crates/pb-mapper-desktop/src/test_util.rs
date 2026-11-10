//! What this crate's own tests share.
use crate::{Host, Pull, VideoCodec, Viewer, ViewerConfig, ViewerStatus};
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

/// Wait until `condition` holds, looking every 10 ms; fail after 10 s.
pub(crate) async fn until(condition: impl FnMut() -> bool) {
    until_within(Duration::from_secs(10), condition).await;
}

/// [`until`], failing after `limit`.
pub(crate) async fn until_within(limit: Duration, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(limit, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// `viewer`'s status once `condition` holds of it, looking as [`until`]
/// does: the status that met it.
pub(crate) async fn status_until(
    viewer: &Viewer,
    condition: impl Fn(&ViewerStatus) -> bool,
) -> ViewerStatus {
    let mut status = viewer.status();
    until(|| {
        status = viewer.status();
        condition(&status)
    })
    .await;
    status
}

/// A renderer on its own thread that presents every unit `viewer` hands
/// it at once, until its sink closes or `stop` is set.
pub(crate) fn present(viewer: &Viewer, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    let consumer = viewer.video_sink().attach().unwrap();
    std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            match consumer.next(Duration::from_millis(20), 4 << 20) {
                Pull::Unit(unit) => {
                    consumer.presented(unit.generation, unit.frame_id);
                }
                Pull::Closed => break,
                Pull::Timeout | Pull::Paused | Pull::NeedCapacity(_) => {}
            }
        }
    })
}

/// A one-pixel desktop that refuses input: enough to hold a viewer's
/// session open.
pub(crate) struct Pixel;
impl crate::Backend for Pixel {
    fn display(&mut self) -> crate::Result<crate::Display> {
        Ok(crate::Display {
            id: 1,
            name: "pixel".into(),
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        })
    }
    fn capture(&mut self) -> crate::Result<Vec<u8>> {
        Ok(vec![1])
    }
    fn input(&mut self, _: &crate::Input) -> crate::Result<()> {
        Err(crate::error("read-only test backend"))
    }
    fn release(&mut self) {}
}

/// An H.264 viewer over the relay whose renderer, on its own thread,
/// presents every unit at once.
pub(crate) struct Presenting {
    pub(crate) viewer: Viewer,
    stop: Arc<AtomicBool>,
    renderer: JoinHandle<()>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let renderer = present(&viewer, stop.clone());
        Self {
            viewer,
            stop,
            renderer,
        }
    }

    /// The viewer's status once `condition` holds.
    pub(crate) async fn until(&self, condition: impl Fn(&ViewerStatus) -> bool) -> ViewerStatus {
        status_until(&self.viewer, condition).await
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
