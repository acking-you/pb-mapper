//! A viewer kept connected by `Supervisor`, through what ends sessions in
//! practice, in one process on any OS.
//!
//! ```text
//! cargo run -p pb-mapper --features desktop --example desktop_supervised
//! ```
//!
//! The host is the synthetic desktop of `desktop_support`, which also makes
//! H.265. Each viewer the supervisor connects meets one event:
//!
//! 1. it is offered H.265, which this renderer cannot decode: after the
//!    decoder errors the session ends, and the supervisor connects again at
//!    once on H.264, never offering H.265 again;
//! 2. the host restarts, as a display switch does: `restarting`, and the
//!    supervisor connects again after its first backoff step, to the same
//!    invitation;
//! 3. the capture hangs: no video for 5 s is a stall, which the supervisor
//!    ends and retries like a timeout;
//! 4. the user goes view-only, which the supervisor leaves so, and the
//!    host stops: nothing to retry, so the supervisor gives up.
//!
//! The control choice and the preset carry over each reconnect, as the
//! viewer last had them. Against your own host, drop the script: `next` and
//! `ended` are the whole loop.

mod desktop_support;

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use desktop_support::{LocalRelay, Panel, Synthetic};
use pb_mapper::ConnectOptions;
use pb_mapper::desktop::{
    Host, HostOptions, QualityPreset, StopReason, VideoCodec, Viewer, ViewerConfig, ViewerState,
    supervisor::Supervisor,
};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::main]
async fn main() -> Outcome {
    let relay = LocalRelay::start("pb-mapper-desktop-supervised").await?;
    let client = relay.client()?;
    let panel = Arc::new(Panel::default());
    let mut options = HostOptions::new("desktop:supervised");
    options.allow_control = true;
    let desktop = panel.clone();
    let mut host = Some(
        Host::builder(options)
            .backend(move || Ok(Synthetic::new(desktop.clone(), true)))
            .start(&client)
            .await?,
    );
    let invitation = host.as_ref().ok_or("no host")?.invitation().clone();

    let mut config = ViewerConfig::new(vec![VideoCodec::H265, VideoCodec::H264]);
    config.quality = QualityPreset::Smooth;
    let mut supervisor = Supervisor::new(client, invitation, ConnectOptions::default(), config);
    supervisor.remember_control(true);

    let started = Instant::now();
    let at = || format!("{:>5.1} s", started.elapsed().as_secs_f32());
    let mut viewers = 0;
    while let Some(attempt) = supervisor.next().await {
        let viewer = match attempt {
            Ok(viewer) => viewer,
            Err(failure) => {
                println!("{} could not connect: {failure}", at());
                continue;
            }
        };
        viewers += 1;
        let renderer = desktop_support::render(&viewer, |codec| codec != VideoCodec::H265);
        let status = viewer.status();
        println!(
            "{} viewer {viewers}: {:?}, {:?}",
            at(),
            status.stats.codec,
            status.quality
        );
        let script = async {
            match viewers {
                2 => {
                    picture_with_control(&viewer).await?;
                    println!("{}   the host restarts", at());
                    let restarting = host.take().ok_or("no host")?;
                    host = Some(restarting.restart(StopReason::Restarting, |_| {}).await?);
                }
                3 => {
                    picture_with_control(&viewer).await?;
                    println!("{}   the capture hangs", at());
                    panel.frozen.store(true, Ordering::Release);
                }
                4 => {
                    picture_with_control(&viewer).await?;
                    // The user's choice is the viewer's: the supervisor
                    // never takes back control the user gave up.
                    println!("{}   the user goes view-only", at());
                    viewer.request_control(false)?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if viewer.status().controlling {
                        return Err("the supervisor took control back".into());
                    }
                    println!("{}   the host stops", at());
                    if let Some(host) = host.take() {
                        host.stop().await?;
                    }
                }
                _ => {}
            }
            Outcome::Ok(())
        };
        let (ended, scripted) = tokio::join!(supervisor.ended(&viewer), script);
        scripted?;
        panel.frozen.store(false, Ordering::Release);
        println!(
            "{}   ended: {:?}{}",
            at(),
            ended.reason,
            match ended.state {
                ViewerState::Stopped => "",
                _ => ", a stall",
            }
        );
        viewer.stop().await?;
        renderer.join().map_err(|_| "the renderer panicked")?;
    }
    println!("{} the supervisor gave up after {viewers} viewers", at());
    relay.stop();
    if viewers != 4 {
        return Err(format!("expected 4 viewers, saw {viewers}").into());
    }
    Ok(())
}

/// Wait for a shown picture and the control the supervisor asks for again.
async fn picture_with_control(viewer: &Viewer) -> Outcome {
    let started = Instant::now();
    loop {
        let status = viewer.status();
        if status.fresh && status.controlling {
            return Ok(());
        }
        if status.state == ViewerState::Stopped || started.elapsed() > Duration::from_secs(10) {
            return Err(format!("no picture under control: {:?}", status.reason).into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
