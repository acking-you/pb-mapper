#![allow(clippy::unwrap_used)]
use super::*;
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_testkit::{Relay, admin_credential};
use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, sync::Notify};

struct LargeFrames {
    count: Arc<AtomicU32>,
    wake: Arc<Notify>,
    configured: bool,
}
impl Backend for LargeFrames {
    fn configure_video(&mut self, limits: VideoLimits, nominal_fps: u32) -> Result<()> {
        limits.validate()?;
        assert_eq!(
            limits,
            VideoLimits {
                max_width: 1280,
                max_height: 720,
                max_fps: 75,
            }
        );
        assert_eq!(nominal_fps, 75);
        self.configured = true;
        Ok(())
    }
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "backpressure".into(),
            x: 0,
            y: 0,
            width: 1600,
            height: 900,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        assert!(self.configured, "video limits must arrive before capture");
        self.count.fetch_add(1, Ordering::Relaxed);
        self.wake.notify_one();
        Ok(vec![0x65; 3 * 1024 * 1024])
    }
    fn input(&mut self, _: &Input) -> Result<()> {
        Ok(())
    }
    fn release(&mut self) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "v1 pipeline; rewritten for v2 after the G1 merges"]
async fn blocked_video_does_not_block_local_stop() {
    let relay = Relay::start("desktop-backpressure").await;
    let client = Client::from_credential(relay.addr().to_string(), admin_credential(), true, None);
    let count = Arc::new(AtomicU32::new(0));
    let wake = Arc::new(Notify::new());
    let (captures, signal) = (count.clone(), wake.clone());
    let host = Host::start_with_backend(
        &client,
        HostOptions {
            service: "desktop:slow".into(),
            display_id: 1,
            allow_control: false,
            allow_direct: false,
        },
        Arc::new(move || {
            Ok(Box::new(LargeFrames {
                count: captures.clone(),
                wake: signal.clone(),
                configured: false,
            }))
        }),
    )
    .await
    .unwrap();
    let (socket, connection) = client
        .connect_tcp(
            host.invitation().service.clone(),
            ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut tls = security::client(host.invitation().fingerprint)
        .unwrap()
        .connect(
            quinn::rustls::pki_types::ServerName::try_from(security::NAME).unwrap(),
            socket,
        )
        .await
        .unwrap();
    wire::write_json(
        &mut tls,
        &wire::Command::Authenticate {
            token: host.invitation().token_text(),
            codecs: vec![],
            video: Some(VideoPreferences {
                max_width: 1280,
                max_height: 720,
                max_fps: 75,
                bitrate: 6_000_000,
            }),
            presentation_feedback: false,
        },
    )
    .await
    .unwrap();
    let (mut read, mut write) = tokio::io::split(tls);
    // A slow reader: ~64 KiB/s, much less than a single 3 MiB frame per second.
    let reader = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(125));
        let mut bytes = [0; 8192];
        loop {
            tick.tick().await;
            if read.read(&mut bytes).await.unwrap_or(0) == 0 {
                break;
            }
        }
    });
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), async {
        while count.load(Ordering::Relaxed) < 12 {
            wake.notified().await;
            wire::write_json(&mut write, &wire::Command::Ping)
                .await
                .unwrap();
        }
    })
    .await
    .unwrap();
    let stopping = Instant::now();
    tokio::time::timeout(Duration::from_secs(2), host.stop())
        .await
        .unwrap()
        .unwrap();
    println!(
        "slow-reader elapsed_ms={} capture_frames={} stop_ms={} frame_limit_bytes={}",
        started.elapsed().as_millis(),
        count.load(Ordering::Relaxed),
        stopping.elapsed().as_millis(),
        wire::MAX_FRAME
    );
    reader.abort();
    let _ = reader.await;
    connection.stop().await.unwrap();
}
