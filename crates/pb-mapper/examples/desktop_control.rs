//! Control a remote desktop: take control, point, click and type.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --features desktop --example desktop_control -- <pbd1.…> \
//!   [--click] [--type TEXT] [--enter]
//! ```
//!
//! This moves the host's real pointer: point it at a host you own, sharing
//! with control allowed. `desktop_loopback --serve` hosts a synthetic
//! desktop that only prints what it receives, which is the safe place to
//! try it. By default it only moves the pointer around the display's
//! centre; `--click` clicks there, `--type` types text, `--enter` presses
//! Enter. The invitation comes from the argument or
//! `PB_MAPPER_DESKTOP_INVITATION`.
//!
//! Input goes in batches of up to 16, in the host display's physical pixels,
//! and only while a picture is current: this example presents every unit as
//! a renderer would once it is on screen. It meets every `InputError`, and
//! releases held keys and buttons on its way out.

use std::error::Error;
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use pb_mapper::desktop::{
    Button, Input, InputError, Invitation, Key, Pull, VideoCodec, Viewer, ViewerConfig,
    ViewerStatus,
};
use pb_mapper::{Client, ClientConfig, ConnectOptions};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::main]
async fn main() -> Outcome {
    let mut invitation = std::env::var("PB_MAPPER_DESKTOP_INVITATION").ok();
    let (mut click, mut text, mut enter) = (false, None, false);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--click" => click = true,
            "--type" => text = Some(args.next().ok_or("--type needs text")?),
            "--enter" => enter = true,
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
    let viewer = Viewer::connect(
        &client,
        &invitation,
        ConnectOptions::default(),
        ViewerConfig::new(vec![VideoCodec::H264, VideoCodec::Jpeg]),
    )
    .await?;
    let consumer = viewer.video_sink().attach()?;
    let presenter = std::thread::spawn(move || {
        let mut capacity = 1 << 20;
        loop {
            match consumer.next(Duration::from_millis(100), capacity) {
                Pull::Unit(unit) => {
                    consumer.presented(unit.generation, unit.frame_id);
                }
                Pull::NeedCapacity(needed) => capacity = needed,
                Pull::Timeout | Pull::Paused => {}
                Pull::Closed => return,
            }
        }
    });

    let controlled = control(&viewer, click, text.as_deref(), enter).await;
    // Whatever happened, let go of every held key and button.
    viewer.release_input()?;
    viewer.stop().await?;
    presenter.join().map_err(|_| "the presenter panicked")?;
    controlled
}

async fn control(viewer: &Viewer, click: bool, text: Option<&str>, enter: bool) -> Outcome {
    let status = until(viewer, "a picture", |status| status.fresh).await?;
    if !status.control_allowed {
        return Err("the host is sharing view-only".into());
    }
    viewer.request_control(true)?;
    until(viewer, "control", |status| status.controlling).await?;
    println!(
        "controlling {}",
        viewer.status().display.map(|d| d.name).unwrap_or_default()
    );

    // A circle around the centre, in display pixels from video fractions.
    for step in 0..=48u8 {
        let angle = f32::from(step) / 48.0 * TAU;
        let view = viewer.view();
        let point = view
            .display_point(0.5 + 0.1 * angle.cos(), 0.5 + 0.1 * angle.sin())
            .ok_or("the host has not described its display")?;
        send(viewer, &[point]).await?;
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    if click {
        let centre = viewer.view().display_point(0.5, 0.5).ok_or("no display")?;
        let press = |down| Input::Button {
            button: Button::Left,
            down,
            drag_start: false,
        };
        // One batch: the move and the click arrive together, in order.
        send(viewer, &[centre, press(true), press(false)]).await?;
    }
    if let Some(text) = text {
        for chunk in Input::text_chunks(text) {
            send(viewer, &[chunk]).await?;
        }
    }
    if enter {
        let key = |down| Input::Key {
            key: Key::Enter,
            down,
        };
        send(viewer, &[key(true), key(false)]).await?;
    }
    // Revoking control, or releasing input, discards input still queued:
    // give the last batch a moment to leave first.
    tokio::time::sleep(Duration::from_millis(250)).await;
    println!("sent; releasing control");
    viewer.request_control(false)?;
    Ok(())
}

/// Queue one batch, meeting every way input can be refused.
async fn send(viewer: &Viewer, batch: &[Input]) -> Outcome {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        match viewer.input(viewer.view().generation, batch) {
            Ok(()) => return Ok(()),
            // No current picture yet (a new generation, a pause): wait for
            // the renderer to present one.
            Err(InputError::NotFresh) => tokio::time::sleep(Duration::from_millis(10)).await,
            // Revoked by the host, or by congestion, which drops queued
            // input: ask again, and send once granted.
            Err(InputError::NotControlling | InputError::Congestion) => {
                viewer.request_control(true)?;
                until(viewer, "control again", |status| status.controlling).await?;
            }
            Err(InputError::Closed) => return Err("the session ended".into()),
            // Outside the display, more than 1024 bytes of text, a wheel
            // beyond ±100: a bug of the sender's.
            Err(InputError::Invalid(why)) => return Err(why.into()),
        }
    }
    Err("input was not accepted within 10 s".into())
}

async fn until(
    viewer: &Viewer,
    what: &str,
    done: impl Fn(&ViewerStatus) -> bool,
) -> Outcome<ViewerStatus> {
    let started = Instant::now();
    loop {
        let status = viewer.status();
        if done(&status) {
            return Ok(status);
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(format!("no {what}: {:?} {:?}", status.state, status.reason).into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
