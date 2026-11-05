//! Explicit transport acceptance, not a decoder/GPU benchmark. Run with:
//! cargo test -p pb-mapper-desktop --all-features sustained_slow_tls_reader -- --ignored --nocapture
#![allow(clippy::unwrap_used)]
use crate::{wire::Command, *};
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy};
use pb_mapper_testkit::Relay;
use std::{
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, sync::Notify};

const FRAME_BYTES: usize = 64 * 1024;
#[derive(Default)]
struct Probe {
    frames: AtomicU32,
    read_bytes: AtomicU64,
    held: AtomicBool,
    changed: Notify,
}
struct SyntheticVideo {
    probe: Arc<Probe>,
    next: Instant,
    adaptive: Option<VideoPreferences>,
}
impl Backend for SyntheticVideo {
    fn configure_video(&mut self, preferences: VideoPreferences) -> Result<()> {
        preferences.validate()?;
        if self.adaptive.is_some() {
            self.adaptive = Some(preferences);
        }
        Ok(())
    }
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "synthetic transport load".into(),
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Err(error("synthetic backend requires the video transport path"))
    }
    fn capture_video(
        &mut self,
        _: &DesktopContext,
        _: VideoCodec,
        keyframe: bool,
        bitrate: u32,
    ) -> Result<Option<EncodedFrame>> {
        if Instant::now() < self.next {
            return Ok(None);
        }
        let fps = self.adaptive.map_or(90, |prefs| prefs.max_fps);
        self.next = Instant::now() + Duration::from_nanos(1_000_000_000 / u64::from(fps));
        let id = self.probe.frames.fetch_add(1, Ordering::Relaxed) + 1;
        self.probe.changed.notify_one();
        // Opaque synthetic access units exercise predictive queue recovery.
        // No decoder consumes them and they must not be used as codec fixtures.
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec: VideoCodec::H264,
                width: self
                    .adaptive
                    .map_or(1280, |prefs| prefs.max_width.min(1280)),
                height: self.adaptive.map_or(720, |prefs| prefs.max_height.min(720)),
                keyframe,
                pts_us: u64::from(id) * 11_111,
                duplicate: false,
            },
            bytes: vec![
                0x65;
                if self.adaptive.is_some() {
                    (bitrate as usize / fps as usize / 8 * if keyframe { 4 } else { 1 })
                        .clamp(256, FRAME_BYTES)
                } else {
                    FRAME_BYTES
                }
            ],
        }))
    }
    fn input(&mut self, _: &Input) -> Result<()> {
        self.probe.held.store(true, Ordering::Release);
        self.probe.changed.notify_one();
        Ok(())
    }
    fn release(&mut self) {
        if self.probe.held.swap(false, Ordering::AcqRel) {
            self.probe.changed.notify_one();
        }
    }
}

async fn observed(probe: &Probe, label: &str, predicate: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let notified = probe.changed.notified();
            if predicate() {
                break;
            }
            notified.await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label} was not observed within one second"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "30-second explicit slow TLS transport acceptance; no GPU or real input"]
async fn sustained_slow_tls_reader() {
    let relay = Relay::start("desktop-sustained-slow-tls").await;
    let (key_id, credential) = relay
        .issue_credential(Duration::from_secs(120), "slow-tls-acceptance")
        .await;
    let client = Client::from_credential(relay.addr().to_string(), credential, true, None);
    let probe = Arc::new(Probe::default());
    let backend_probe = probe.clone();
    let host = Host::start_with_backend(
        &client,
        HostOptions {
            service: "desktop:slow-tls".into(),
            display_id: 1,
            allow_control: true,
            allow_direct: false,
        },
        Arc::new(move || {
            Ok(Box::new(SyntheticVideo {
                probe: backend_probe.clone(),
                next: Instant::now(),
                adaptive: None,
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
    let mut tls = security::client(&host.invitation().certificate)
        .unwrap()
        .connect(
            quinn::rustls::pki_types::ServerName::try_from(security::NAME).unwrap(),
            socket,
        )
        .await
        .unwrap();
    wire::write_json(
        &mut tls,
        &Command::Authenticate {
            token: host.invitation().token.clone(),
            codecs: vec![VideoCodec::H264],
            video: Some(VideoPreferences::default()),
            presentation_feedback: false,
        },
    )
    .await
    .unwrap();
    let (mut read, mut write) = tokio::io::split(tls);
    let read_probe = probe.clone();
    let reader = tokio::spawn(async move {
        // Throttle the actual TLS reader. Unlike a slow Viewer::next_frame
        // consumer, nothing in the background drains and discards this stream.
        // 256 KiB/s is well below the synthetic source, while allowing TCP
        // window updates to complete within the production 3-second I/O limit.
        let mut tick = tokio::time::interval(Duration::from_micros(62_500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut bytes = [0; 16 * 1024];
        loop {
            tick.tick().await;
            let count = read.read(&mut bytes).await.unwrap();
            assert!(count > 0, "slow transport disconnected before local stop");
            read_probe
                .read_bytes
                .fetch_add(count as u64, Ordering::Relaxed);
        }
    });
    let start = Instant::now();
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let mut control_ms = 0.0;
    let mut release_ms = 0.0;
    let mut revoke_ms = 0.0;
    let mut halfway_frames = 0;
    for second in 1..=30 {
        tick.tick().await;
        assert!(!reader.is_finished(), "TLS reader failed");
        tokio::time::timeout(
            Duration::from_secs(1),
            wire::write_json(&mut write, &Command::Ping),
        )
        .await
        .unwrap()
        .unwrap();
        let state = host.status();
        assert_eq!(
            state.state, "streaming",
            "host must remain active for all 30 seconds"
        );
        if second == 15 {
            halfway_frames = probe.frames.load(Ordering::Relaxed);
            assert!(
                state.dropped_frames > 0,
                "reader must exert real backpressure"
            );
            let at = Instant::now();
            wire::write_json(&mut write, &Command::Control { enabled: true })
                .await
                .unwrap();
            observed(&probe, "control enabled", || host.status().controlling).await;
            control_ms = at.elapsed().as_secs_f64() * 1000.0;
            let generation_frame = host.status().frame_id;
            observed(&probe, "new control generation's first frame", || {
                host.status().frame_id > generation_frame
            })
            .await;
            for sequence in 1..=2 {
                // The synthetic backend injects no OS input. Use a known fresh
                // host frame to isolate command responsiveness from staleness.
                let state = host.status();
                wire::write_json(
                    &mut write,
                    &Command::Input {
                        event: InputEvent {
                            generation: state.generation,
                            frame_id: state.frame_id,
                            sequence,
                            input: Input::Button {
                                button: Button::Left,
                                down: true,
                            },
                        },
                    },
                )
                .await
                .unwrap();
                observed(&probe, "synthetic input", || {
                    probe.held.load(Ordering::Acquire)
                })
                .await;
                let at = Instant::now();
                if sequence == 1 {
                    wire::write_json(&mut write, &Command::Release)
                        .await
                        .unwrap();
                } else {
                    host.set_control_allowed(false);
                }
                observed(&probe, "release or revoke", || {
                    !probe.held.load(Ordering::Acquire)
                })
                .await;
                if sequence == 1 {
                    release_ms = at.elapsed().as_secs_f64() * 1000.0;
                } else {
                    revoke_ms = at.elapsed().as_secs_f64() * 1000.0;
                }
            }
        }
        if second % 10 == 0 {
            println!(
                "slow_tls seconds={second} captured={} dropped={} read_bytes={}",
                probe.frames.load(Ordering::Relaxed),
                state.dropped_frames,
                probe.read_bytes.load(Ordering::Relaxed)
            );
        }
    }
    let state = host.status();
    let elapsed = start.elapsed().as_secs_f64();
    assert!(
        probe.frames.load(Ordering::Relaxed) > halfway_frames + 300,
        "capture must continue after control revocation"
    );
    assert!(state.dropped_frames > 0);
    assert!(!state.controlling);
    let stopping = Instant::now();
    tokio::time::timeout(Duration::from_secs(1), host.stop())
        .await
        .unwrap()
        .unwrap();
    let stop_ms = stopping.elapsed().as_secs_f64() * 1000.0;
    reader.abort();
    let _ = reader.await;
    connection.stop().await.unwrap();
    relay.revoke(key_id).await;
    println!(
        "slow_tls elapsed_s={elapsed:.3} reader_kib_s={:.2} captured={} dropped={} queue_limit_frames={} queue_limit_bytes={} synthetic_frame_bytes={FRAME_BYTES} control_ms={control_ms:.3} release_ms={release_ms:.3} revoke_ms={revoke_ms:.3} stop_ms={stop_ms:.3}",
        probe.read_bytes.load(Ordering::Relaxed) as f64 / elapsed / 1024.0,
        probe.frames.load(Ordering::Relaxed),
        state.dropped_frames,
        crate::media::MAX_QUEUED_FRAMES,
        crate::media::MAX_QUEUED_BYTES
    );
}

struct PacedRead {
    start: Instant,
    next: tokio::time::Instant,
    bytes: u64,
}
impl PacedRead {
    async fn exact(
        &mut self,
        reader: &mut (impl tokio::io::AsyncRead + Unpin),
        mut bytes: &mut [u8],
    ) -> Result<()> {
        while !bytes.is_empty() {
            let length = bytes.len().min(4096);
            let count = reader.read(&mut bytes[..length]).await?;
            if count == 0 {
                return Err(error("feedback test stream ended"));
            }
            self.bytes += count as u64;
            if self.start.elapsed() < Duration::from_secs(8) {
                self.next += Duration::from_secs_f64(count as f64 / (100.0 * 1024.0));
                tokio::time::sleep_until(self.next).await;
            } else {
                self.next = tokio::time::Instant::now();
            }
            bytes = &mut bytes[count..];
        }
        Ok(())
    }
    async fn message(
        &mut self,
        reader: &mut (impl tokio::io::AsyncRead + Unpin),
        limit: usize,
    ) -> Result<Vec<u8>> {
        let mut length = [0; 4];
        self.exact(reader, &mut length).await?;
        let length = u32::from_be_bytes(length) as usize;
        if length > limit {
            return Err(error("oversized feedback test packet"));
        }
        let mut bytes = vec![0; length];
        self.exact(reader, &mut bytes).await?;
        Ok(bytes)
    }
}

/// The renderer is a deliberately delayed test stand-in; no claim of real GPU
/// presentation is made. ACKs occur only after the full synthetic access unit
/// has traversed the actual TLS reader and the simulated presentation deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit ~35-second low-bandwidth/slow-presentation recovery acceptance"]
async fn presentation_feedback_recovers_after_slow_tls_and_renderer() {
    let relay = Relay::start("desktop-presentation-feedback").await;
    let (key_id, credential) = relay
        .issue_credential(Duration::from_secs(120), "feedback-acceptance")
        .await;
    let client = Client::from_credential(relay.addr().to_string(), credential, true, None);
    let probe = Arc::new(Probe::default());
    let source = probe.clone();
    let host = Host::start_with_backend(
        &client,
        HostOptions {
            service: "desktop:feedback".into(),
            display_id: 1,
            allow_control: false,
            allow_direct: false,
        },
        Arc::new(move || {
            Ok(Box::new(SyntheticVideo {
                probe: source.clone(),
                next: Instant::now(),
                adaptive: Some(VideoPreferences::default()),
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
    let mut tls = security::client(&host.invitation().certificate)
        .unwrap()
        .connect(
            quinn::rustls::pki_types::ServerName::try_from(security::NAME).unwrap(),
            socket,
        )
        .await
        .unwrap();
    wire::write_json(
        &mut tls,
        &Command::Authenticate {
            token: host.invitation().token.clone(),
            codecs: vec![VideoCodec::H264],
            video: Some(VideoPreferences::default()),
            presentation_feedback: true,
        },
    )
    .await
    .unwrap();
    let (mut read, write) = tokio::io::split(tls);
    let write = Arc::new(tokio::sync::Mutex::new(write));
    let (received, newest) = tokio::sync::watch::channel((0u64, 0u64));
    let observed = Arc::new(parking_lot::Mutex::new(SessionStatus::default()));
    let observation = observed.clone();
    let start = Instant::now();
    let reader = tokio::spawn(async move {
        let mut pace = PacedRead {
            start,
            next: tokio::time::Instant::now(),
            bytes: 0,
        };
        loop {
            let state: SessionStatus =
                serde_json::from_slice(&pace.message(&mut read, wire::MAX_JSON).await?)
                    .map_err(error)?;
            let image = pace.message(&mut read, wire::MAX_FRAME).await?;
            if state.state == "stopped" {
                return Ok::<_, std::io::Error>((state, pace.bytes));
            }
            if !image.is_empty() {
                received.send_replace((state.generation, state.frame_id));
            }
            *observation.lock() = state;
        }
    });
    let acknowledgment_write = write.clone();
    let acknowledgments = tokio::spawn(async move {
        let mut last = (0, 0);
        loop {
            let elapsed = start.elapsed();
            let delay = if elapsed < Duration::from_secs(8) {
                100
            } else if elapsed < Duration::from_secs(12) {
                600
            } else {
                20
            };
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let current = *newest.borrow();
            if current == last || current.1 == 0 {
                continue;
            }
            wire::write_json(
                &mut *acknowledgment_write.lock().await,
                &Command::Presented {
                    generation: current.0,
                    frame_id: current.1,
                },
            )
            .await?;
            last = current;
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    });
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let mut minimum_bitrate = u32::MAX;
    let mut maximum_ack_ms = 0f32;
    for second in 1..=30 {
        tick.tick().await;
        assert!(
            !reader.is_finished(),
            "feedback session ended before recovery: {:?}",
            host.status()
        );
        assert!(!acknowledgments.is_finished());
        wire::write_json(&mut *write.lock().await, &Command::Ping)
            .await
            .unwrap();
        let state = observed.lock().clone();
        if state.bitrate > 0 {
            minimum_bitrate = minimum_bitrate.min(state.bitrate);
        }
        maximum_ack_ms = maximum_ack_ms.max(state.frame_ack_ms);
        if [8, 12, 20, 30].contains(&second) {
            println!(
                "presentation_feedback second={second} bitrate={} video={}x{} source_fps={:.2} ack_ms={:.2} frame={} dropped={}",
                state.bitrate,
                state.video.width,
                state.video.height,
                state.capture_fps,
                state.frame_ack_ms,
                state.frame_id,
                state.dropped_frames
            );
        }
        if second == 8 {
            assert!(
                minimum_bitrate < 1_000_000,
                "must react to 100KiB/s before socket buffers fill"
            );
        }
    }
    let recovered = observed.lock().clone();
    assert!(
        recovered.bitrate >= 4_000_000,
        "fast presentation must recover quality: {recovered:?}"
    );
    assert!(recovered.video.width >= 960);
    assert!(recovered.frame_ack_ms < 350.0);
    acknowledgments.abort();
    let _ = acknowledgments.await;
    // A renderer which stops presenting must end with an explicit reason. It
    // must neither receive an unbounded video backlog nor freeze forever.
    let no_ack = Instant::now();
    let finished = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if reader.is_finished() {
                break;
            }
            wire::write_json(&mut *write.lock().await, &Command::Ping)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        reader.await.unwrap().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(finished.0.state, "stopped");
    assert!(!finished.0.message.is_empty());
    assert!(no_ack.elapsed() < Duration::from_secs(6));
    host.stop().await.unwrap();
    connection.stop().await.unwrap();
    relay.revoke(key_id).await;
    println!(
        "presentation_feedback accepted minimum_bitrate={minimum_bitrate} recovered_bitrate={} max_valid_ack_ms={maximum_ack_ms:.2} no_ack_stop_ms={:.2} tls_bytes={} final_reason={}",
        recovered.bitrate,
        no_ack.elapsed().as_secs_f64() * 1000.0,
        finished.1,
        finished.0.message
    );
}
