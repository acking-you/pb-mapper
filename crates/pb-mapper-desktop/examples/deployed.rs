//! Acceptance against a configured deployed relay. Never print credentials.
//! The exported controller setup contains a dedicated expiring test credential,
//! never the administrator credential, and is revoked when the host exits.
//! `selftest` views the host from this machine through a simulated renderer
//! that presents every unit at once. It asks for the display's own size, as
//! a viewer does by default; options `--max-width`, `--max-height`, `--fps`
//! and `--quality quality|balanced|smooth` set what it asks instead,
//! `--fps-override` makes `--fps` its user's choice, every preset's ceiling,
//! and `--codec h265` offers H.265 beside H.264, as the app does when it can
//! decode it (the host picks; the line printed says what it picked).
//! `--no-decoder-hints` has the phone tool configure its decoder without the
//! low-latency hint (case F). The configuration's `lan_first`, the app's
//! *Try the local network first*, is read once at start: the host and the
//! selftest viewer take it, and `controller.json` hands it to the phone tool.
mod probe_support;

use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy, RelayTransport};
use pb_mapper_desktop::{
    DisplaySelection, Host, HostOptions, Pull, QualityPreset, Resolution, VideoCodec, VideoLimits,
    Viewer, ViewerConfig, ViewerState,
};
use probe_support::{option, relay_client};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Outcome<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
struct Configuration {
    server_address: String,
    msg_header_key: String,
    #[serde(default)]
    relay_transport: RelayTransport,
    /// As the app's `AppConfig::lan_first`, so on for a file saved before
    /// the switch existed.
    #[serde(default = "probe_support::lan_first_by_default")]
    lan_first: bool,
}

/// What the command line asks for after the configuration path.
struct Run {
    output: PathBuf,
    /// `serve` hosts until `output/stop` appears; anything else is a selftest.
    mode: String,
    seconds: u64,
    limits: VideoLimits,
    quality: QualityPreset,
    codecs: Vec<VideoCodec>,
    fps_override: bool,
    /// The host allows control.
    control: bool,
    /// The phone tool configures its decoder with the low-latency hint.
    decoder_hints: bool,
}
impl Run {
    fn parse(args: &[String]) -> Outcome<Self> {
        let output = PathBuf::from(args.get(2).ok_or("output directory required")?);
        let mode = args.get(3).map_or("selftest", String::as_str).to_owned();
        let seconds: u64 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(20);
        if !(5..=1800).contains(&seconds) {
            return Err("duration outside 5..1800 seconds".into());
        }
        let (max_width, max_height) = Resolution::default().bounds();
        let mut limits = VideoLimits {
            max_width,
            max_height,
            max_fps: 60,
        };
        for (name, value) in [
            ("--max-width", &mut limits.max_width),
            ("--max-height", &mut limits.max_height),
            ("--fps", &mut limits.max_fps),
        ] {
            if let Some(text) = option(args, name) {
                *value = text.parse()?;
            }
        }
        limits.validate()?;
        let quality = match option(args, "--quality") {
            None | Some("balanced") => QualityPreset::Balanced,
            Some("quality") => QualityPreset::Quality,
            Some("smooth") => QualityPreset::Smooth,
            Some(_) => return Err("--quality must be quality, balanced or smooth".into()),
        };
        let codecs = match option(args, "--codec") {
            None | Some("h264") => vec![VideoCodec::H264],
            Some("h265") => vec![VideoCodec::H265, VideoCodec::H264],
            Some(_) => return Err("--codec must be h264 or h265".into()),
        };
        let flag = |name: &str| args.iter().any(|arg| arg == name);
        Ok(Self {
            output,
            mode,
            seconds,
            limits,
            quality,
            codecs,
            fps_override: flag("--fps-override"),
            control: flag("--control"),
            decoder_hints: !flag("--no-decoder-hints"),
        })
    }
}

#[tokio::main]
async fn main() -> Outcome {
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
    let run = Run::parse(&args)?;
    std::fs::create_dir_all(&run.output)?;
    let admin_client = relay_client(
        config.server_address.clone(),
        config.msg_header_key.clone(),
        config.relay_transport,
    )?;
    let admin = admin_client.admin()?;
    let key = admin
        .issue_key(
            Duration::from_secs(run.seconds + 300),
            Some("desktop-acceptance-ephemeral".into()),
        )
        .await?;
    let result = host_and_run(&config, &key.credential, &run).await;
    let revoke = admin.revoke_key(key.key_id).await;
    let _ = std::fs::remove_file(run.output.join("controller.json"));
    revoke?;
    result
}

/// Host the first display under the test credential, hand the phone tool its
/// setup in `controller.json`, then serve or measure.
async fn host_and_run(config: &Configuration, credential: &str, run: &Run) -> Outcome {
    let client = relay_client(
        config.server_address.clone(),
        credential.to_owned(),
        config.relay_transport,
    )?;
    let display = pb_mapper_desktop::displays()?
        .into_iter()
        .next()
        .ok_or("no display")?;
    let mut options = HostOptions::new(format!("desktop:acceptance-{}", std::process::id()));
    options.display = DisplaySelection::Id(display.id);
    options.allow_control = run.control;
    options.lan_first = config.lan_first;
    let host = Host::start(&client, options).await?;
    // The phone tool follows `lan_first` as it follows the carrier, so both
    // ends run the host's arm and the report records it.
    let controller = controller_json(
        config,
        credential,
        host.invitation().encode(),
        run.decoder_hints,
    );
    std::fs::write(
        run.output.join("controller.json"),
        serde_json::to_vec(&controller)?,
    )?;
    println!(
        "host_ready display={}x{} duration={} mode={} lan_first={}",
        display.width, display.height, run.seconds, run.mode, config.lan_first
    );
    if run.mode == "serve" {
        serve(&host, &run.output, run.seconds).await;
    } else {
        for policy in [PathPolicy::Auto, PathPolicy::RelayOnly] {
            measure(&client, &host, policy, config.lan_first, run).await?;
        }
    }
    host.stop().await?;
    Ok(())
}

/// The phone tool's setup: the relay, the test credential, the carrier,
/// LAN-first and the invitation.
fn controller_json(
    config: &Configuration,
    credential: &str,
    invitation: String,
    decoder_hints: bool,
) -> serde_json::Value {
    serde_json::json!({
        "server": config.server_address,
        "credential": credential,
        "transport": config.relay_transport,
        "lan_first": config.lan_first,
        "invitation": invitation,
        "decoder_hints": decoder_hints,
    })
}

/// The host's state once a second for `seconds`, or until `output/stop`
/// appears.
async fn serve(host: &Host, output: &Path, seconds: u64) {
    for _ in 0..seconds {
        if output.join("stop").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status = host.status();
        println!(
            "state={:?} viewer={:?} message={}",
            status.state, status.viewer, status.message
        );
    }
}

/// View the host over `path` for the run's seconds through a renderer that
/// presents every unit at once, and print what the session measured.
async fn measure(
    client: &Client,
    host: &Host,
    path: PathPolicy,
    lan_first: bool,
    run: &Run,
) -> Outcome {
    let options = ConnectOptions {
        path,
        lan_first,
        ..Default::default()
    };
    let config = ViewerConfig {
        codecs: run.codecs.clone(),
        limits: run.limits,
        fps_override: run.fps_override,
        quality: run.quality,
        progress: None,
    };
    let viewer = Viewer::connect(client, host.invitation(), options, config).await?;
    let consumer = viewer.video_sink().attach()?;
    let seconds = run.seconds;
    // The renderer blocks on the sink, so it gets its own thread.
    let renderer = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let (mut received, mut bytes) = (0u64, 0u64);
        let mut capacity = 4 << 20;
        while start.elapsed() < Duration::from_secs(seconds) {
            match consumer.next(Duration::from_millis(100), capacity) {
                Pull::Unit(unit) => {
                    received += u64::from(!unit.duplicate);
                    bytes += unit.bytes.len() as u64;
                    consumer.presented(unit.generation, unit.frame_id);
                }
                Pull::NeedCapacity(needed) => capacity = needed,
                Pull::Timeout | Pull::Paused => {}
                Pull::Closed => break,
            }
        }
        (received, bytes, start.elapsed().as_secs_f64())
    });
    let (received, bytes, elapsed) = renderer.await?;
    let status = viewer.status();
    if matches!(
        status.state,
        ViewerState::Stopped | ViewerState::CaptureUnavailable
    ) {
        let _ = viewer.stop().await;
        return Err(format!("{:?}: {}", status.reason, status.message).into());
    }
    let stats = &status.stats;
    let fps_override = run.fps_override;
    println!(
        "path={} codec={:?} video={}x{} fps_limit={} fps_override={fps_override} unique_receive_fps={:.2} presented_fps={:.2} source_fps={:.2} receive_mbps={:.2} speed_rx_bps={} speed_tx_bps={} dropped={} picture_latency_ms={:?}",
        stats.path,
        stats.codec,
        stats.width,
        stats.height,
        run.limits.max_fps,
        received as f64 / elapsed,
        stats.presented_fps,
        stats.capture_fps,
        bytes as f64 * 8.0 / elapsed / 1e6,
        stats.speed_rx_bps,
        stats.speed_tx_bps,
        stats.dropped_frames,
        stats.picture_latency_ms
    );
    viewer.stop().await?;
    Ok(())
}
