//! View a remote desktop and record its picture to a file you can play.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --features desktop --example desktop_viewer -- <pbd1.…>
//! ffplay -i remote.h264
//! ```
//!
//! The invitation comes from the argument or `PB_MAPPER_DESKTOP_INVITATION`.
//! Options: `--resolution 720p|1080p|1440p|native`, `--quality
//! quality|balanced|smooth`, `--relay-only`, `--lan-transport quic|tcp`,
//! `--seconds N` (else until Ctrl+C), `--out FILE`.
//!
//! The SDK ships no decoder: it hands out H.264 access units in Annex B, and
//! the integrator decodes them. This example stands in for a renderer on its
//! own thread. It starts at a keyframe, writes every unit to the file, and
//! reports each one "presented"; a real renderer reports a frame only once
//! it is on screen, since input is authorised only by a recent
//! presentation. It never sends input.

use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Duration;

use pb_mapper::desktop::{
    ConnectError, Invitation, Pull, QualityPreset, Resolution, VideoCodec, VideoLimits, Viewer,
    ViewerConfig, ViewerState,
};
use pb_mapper::{Client, ClientConfig, ConnectOptions, LanTransport, PathPolicy};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::main]
async fn main() -> Outcome {
    let mut invitation = std::env::var("PB_MAPPER_DESKTOP_INVITATION").ok();
    let mut resolution = Resolution::P1080;
    let mut quality = QualityPreset::Balanced;
    let mut options = ConnectOptions::default();
    let mut seconds = None;
    let mut out = "remote.h264".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--resolution" => {
                resolution = match value()?.as_str() {
                    "720p" => Resolution::P720,
                    "1080p" => Resolution::P1080,
                    "1440p" => Resolution::P1440,
                    "native" => Resolution::Native,
                    other => return Err(format!("unknown resolution {other}").into()),
                }
            }
            "--quality" => {
                quality = match value()?.as_str() {
                    "quality" => QualityPreset::Quality,
                    "balanced" => QualityPreset::Balanced,
                    "smooth" => QualityPreset::Smooth,
                    other => return Err(format!("unknown preset {other}").into()),
                }
            }
            "--relay-only" => options.path = PathPolicy::RelayOnly,
            "--lan-transport" => {
                options.lan_transport = match value()?.as_str() {
                    "quic" => LanTransport::Quic,
                    "tcp" => LanTransport::Tcp,
                    other => return Err(format!("unknown LAN transport {other}").into()),
                }
            }
            "--seconds" => seconds = Some(value()?.parse::<u64>()?),
            "--out" => out = value()?,
            _ if arg.starts_with("pbd1.") => invitation = Some(arg),
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let invitation = Invitation::decode(
        invitation
            .as_deref()
            .ok_or("pass a pbd1. invitation, or set PB_MAPPER_DESKTOP_INVITATION")?,
    )?;
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: true,
        namespace: None,
    })?;

    // H.264 only, at this display's frame rate within the chosen bounds.
    let mut config = ViewerConfig::new(vec![VideoCodec::H264]);
    config.limits = VideoLimits::choose(60.0, resolution, None);
    config.quality = quality;
    println!("connecting to {}", invitation.service);
    let viewer = match Viewer::connect(&client, &invitation, options, config).await {
        Ok(viewer) => viewer,
        Err(failure) => {
            let advice = match &failure {
                ConnectError::IdentityChanged => {
                    "the host's identity changed: ask for a new invitation"
                }
                ConnectError::VersionMismatch => "update pb-mapper on both computers",
                ConnectError::Busy => "another viewer is connected; try again later",
                ConnectError::Denied => "the host refused this invitation: it was revoked",
                ConnectError::InvalidRequest => "the invitation or the video settings are invalid",
                ConnectError::Cancelled => "the attempt was cancelled",
                ConnectError::Transport(_) => "the host is not reachable now: is it sharing?",
                ConnectError::Protocol(_) => "the host answered something unexpected",
            };
            return Err(format!("{failure} ({advice})").into());
        }
    };

    let consumer = viewer.video_sink().attach()?;
    let mut file = BufWriter::new(File::create(&out)?);
    let recorder = std::thread::spawn(move || -> std::io::Result<u64> {
        let (mut capacity, mut written, mut started) = (1 << 20, 0, false);
        loop {
            match consumer.next(Duration::from_millis(100), capacity) {
                Pull::Unit(unit) => {
                    // A decoder starts at a keyframe; after a discontinuity
                    // the sink hands out an IDR next.
                    started |= unit.keyframe;
                    if !started {
                        continue;
                    }
                    file.write_all(&unit.bytes)?;
                    written += unit.bytes.len() as u64;
                    consumer.presented(unit.generation, unit.frame_id);
                }
                Pull::NeedCapacity(needed) => capacity = needed,
                Pull::Timeout | Pull::Paused => {}
                Pull::Closed => {
                    file.flush()?;
                    return Ok(written);
                }
            }
        }
    });

    // Each change of state, the current one first.
    let mut status = viewer.watch(Duration::from_millis(250));
    status.mark_changed();
    let mut every = tokio::time::interval(Duration::from_secs(1));
    let deadline = async {
        match seconds {
            Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            changed = status.changed() => {
                if changed.is_err() {
                    break;
                }
                let status = status.borrow_and_update().clone();
                println!(
                    "{:?}{}{}",
                    status.state,
                    status.reason.map(|reason| format!(" ({reason:?})")).unwrap_or_default(),
                    status.video.map(|video| format!(", {:?} {}x{}", video.codec, video.width, video.height)).unwrap_or_default(),
                );
                if status.state == ViewerState::Stopped {
                    break;
                }
            }
            _ = every.tick() => {
                let stats = viewer.status().stats;
                let direct = viewer.metrics().map(|metrics| metrics.sessions.active_direct > 0);
                println!(
                    "  {} {:.0} fps, latency {:?} ms, {} kbit/s{}",
                    stats.path,
                    stats.presented_fps,
                    stats.picture_latency_ms,
                    stats.received_bitrate / 1000,
                    if direct == Some(true) { ", direct" } else { "" },
                );
            }
            result = tokio::signal::ctrl_c() => { result?; break; }
            () = &mut deadline => break,
        }
    }
    viewer.stop().await?;
    let written = recorder.join().map_err(|_| "the recorder panicked")??;
    println!("wrote {written} bytes of H.264 to {out}; play it with `ffplay -i {out}`");
    Ok(())
}
