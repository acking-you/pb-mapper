//! Acceptance against a configured deployed relay. Never print credentials.
//! The exported controller setup contains a dedicated expiring test credential,
//! never the administrator credential, and is revoked when the host exits.
//! `selftest` views the host from this machine through a simulated renderer
//! that presents every unit at once; options `--max-width`, `--max-height`,
//! `--fps` and `--quality quality|balanced|smooth` set what that viewer asks.
use pb_mapper_client::sdk::{Client, ClientConfig, ConnectOptions, PathPolicy, RelayTransport};
use pb_mapper_desktop::{
    Host, HostOptions, Pull, QualityPreset, VideoCodec, VideoLimits, Viewer, ViewerConfig,
    ViewerState,
};
use serde::Deserialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Configuration {
    server_address: String,
    msg_header_key: String,
    #[serde(default)]
    relay_transport: RelayTransport,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--feedback-log") {
        // Deliberately ignore RUST_LOG: these diagnostics expose only feedback
        // counters and per-frame timing, never SDK registration, invitation or
        // credential details.
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "off,pb_mapper_desktop::feedback=debug,pb_mapper_desktop::frames=trace",
            ))
            .with_ansi(false)
            .try_init()
            .map_err(|error| format!("feedback logging initialization failed: {error}"))?;
    }
    let mut config: Configuration = serde_json::from_slice(&std::fs::read(
        args.get(1).ok_or("configuration path required")?,
    )?)?;
    if args.iter().any(|s| s == "--tcp") {
        config.relay_transport = RelayTransport::Tcp;
    }
    let output = PathBuf::from(args.get(2).ok_or("output directory required")?);
    let mode = args.get(3).map(String::as_str).unwrap_or("selftest");
    let seconds: u64 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(20);
    if !(5..=1800).contains(&seconds) {
        return Err("duration outside 5..1800 seconds".into());
    }
    let option = |name| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    };
    let mut limits = VideoLimits {
        max_width: 1920,
        max_height: 1080,
        max_fps: 60,
    };
    for (name, value) in [
        ("--max-width", &mut limits.max_width),
        ("--max-height", &mut limits.max_height),
        ("--fps", &mut limits.max_fps),
    ] {
        if let Some(text) = option(name) {
            *value = text.parse()?;
        }
    }
    limits.validate()?;
    let quality = match option("--quality") {
        None | Some("balanced") => QualityPreset::Balanced,
        Some("quality") => QualityPreset::Quality,
        Some("smooth") => QualityPreset::Smooth,
        Some(_) => return Err("--quality must be quality, balanced or smooth".into()),
    };
    std::fs::create_dir_all(&output)?;
    let admin_client = Client::new(ClientConfig {
        server: config.server_address.clone(),
        credential: config.msg_header_key,
        keep_alive: true,
        namespace: None,
    })?
    .with_relay_transport(config.relay_transport)?;
    let admin = admin_client.admin()?;
    let key = admin
        .issue_key(
            Duration::from_secs(seconds + 300),
            Some("desktop-acceptance-ephemeral".into()),
        )
        .await?;
    let result = async {
        let client = Client::new(ClientConfig { server: config.server_address.clone(), credential: key.credential.clone(), keep_alive: true, namespace: None })?.with_relay_transport(config.relay_transport)?;
        let display = pb_mapper_desktop::displays()?.into_iter().next().ok_or("no display")?;
        let host = Host::start(&client, HostOptions { service: format!("desktop:acceptance-{}",std::process::id()), display_id:display.id, allow_control:args.iter().any(|arg| arg == "--control"),allow_direct:true }).await?;
        let controller = serde_json::json!({"server":config.server_address,"credential":key.credential,"transport":config.relay_transport,"invitation":host.invitation().encode()});
        std::fs::write(output.join("controller.json"),serde_json::to_vec(&controller)?)?;
        println!("host_ready display={}x{} duration={seconds} mode={mode}", display.width,display.height);
        if mode == "serve" {
            for _ in 0..seconds {
                if output.join("stop").exists() { break; }
                tokio::time::sleep(Duration::from_secs(1)).await;
                let s=host.status();
                println!("state={:?} viewer={:?} message={}",s.state,s.viewer,s.message);
            }
        } else {
            for policy in [PathPolicy::Auto,PathPolicy::RelayOnly] {
                let viewer = Viewer::connect(&client,host.invitation(),ConnectOptions { path: policy, ..Default::default() },ViewerConfig { codecs: vec![VideoCodec::H264], limits, quality, progress: None }).await?;
                let consumer = viewer.video_sink().attach()?;
                // The renderer blocks on the sink, so it gets its own thread.
                let renderer = tokio::task::spawn_blocking(move || {
                    let start = Instant::now(); let (mut received, mut bytes) = (0u64, 0u64);
                    while start.elapsed() < Duration::from_secs(seconds) {
                        match consumer.next(Duration::from_millis(100), 4 << 20) {
                            Pull::Unit(unit) => {
                                received += u64::from(!unit.duplicate);
                                bytes += unit.bytes.len() as u64;
                                consumer.presented(unit.generation, unit.frame_id);
                            }
                            Pull::Closed => break,
                            _ => {}
                        }
                    }
                    (received, bytes, start.elapsed().as_secs_f64())
                });
                let (received, bytes, elapsed) = renderer.await?;
                let status = viewer.status();
                if matches!(status.state, ViewerState::Stopped | ViewerState::CaptureUnavailable) {
                    let _ = viewer.stop().await;
                    return Err(format!("{:?}: {}", status.reason, status.message).into());
                }
                let stats = &status.stats;
                println!("path={} video={}x{} fps_limit={} unique_receive_fps={:.2} presented_fps={:.2} source_fps={:.2} receive_mbps={:.2} dropped={} picture_latency_ms={:?}", stats.path,stats.width,stats.height,limits.max_fps,received as f64/elapsed,stats.presented_fps,stats.capture_fps, bytes as f64*8.0/elapsed/1e6,stats.dropped_frames,stats.picture_latency_ms);
                viewer.stop().await?;
            }
        }
        host.stop().await?;
        Ok::<_,Box<dyn std::error::Error>>(())
    }.await;
    let revoke = admin.revoke_key(key.key_id).await;
    let _ = std::fs::remove_file(output.join("controller.json"));
    revoke?;
    result
}
