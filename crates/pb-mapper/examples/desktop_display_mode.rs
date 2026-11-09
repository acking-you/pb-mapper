//! List a remote computer's display modes, change one, and see it put back.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --features desktop --example desktop_display_mode -- <pbd1.…> \
//!   [--fit WxH | --mode WxH | --original] [--hold SECONDS]
//! ```
//!
//! Without an option it only lists the host display's modes. `--fit WxH`
//! asks for the mode that best fits a viewer display of that size, as a
//! desktop app's "Fit local" does; `--mode WxH` for one the host lists;
//! `--original` for the mode the host found. The invitation comes from the
//! argument or `PB_MAPPER_DESKTOP_INVITATION`.
//!
//! This changes the real mode of the host's screen, for as long as this
//! viewer stays (`--hold`, 5 s by default): point it at a host you own,
//! sharing with control allowed, since only a controlling viewer may change
//! modes. `desktop_loopback --serve` hosts a synthetic desktop that only
//! prints its mode, which is the safe place to try it. The host puts its
//! display back when the viewer disconnects (or when control is revoked),
//! and this example connects once more to show that it did.

use std::error::Error;
use std::time::{Duration, Instant};

use pb_mapper::desktop::{
    Decoders, DisplayModeError, Invitation, Mode, ModeList, ModeTarget, NoticeCode, Pull,
    Resolution, Viewer, ViewerConfig, ViewerStatus,
};
use pb_mapper::{Client, ClientConfig, ConnectOptions};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::main]
async fn main() -> Outcome {
    let mut invitation = std::env::var("PB_MAPPER_DESKTOP_INVITATION").ok();
    let mut target = None;
    let mut hold = Duration::from_secs(5);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--fit" => target = Some(ModeTarget::FitLocal(size(&value()?)?)),
            "--mode" => target = Some(ModeTarget::Size(size(&value()?)?)),
            "--original" => target = Some(ModeTarget::Original),
            "--hold" => hold = Duration::from_secs(value()?.parse()?),
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

    let viewer = connect(&client, &invitation).await?;
    let status = until(&viewer, "the host's display modes", |status| {
        status.modes.is_some() || !status.capabilities.iter().any(|c| c == "display_mode")
    })
    .await?;
    let Some(modes) = status.modes else {
        viewer.stop().await?;
        return Err("this host offers no display modes".into());
    };
    print_modes(&modes);
    let Some(target) = target else {
        viewer.stop().await?;
        return Ok(());
    };

    // Only a controlling viewer changes modes.
    viewer.request_control(true)?;
    let status = until(&viewer, "control", |status| {
        status.controlling || (status.fresh && !status.control_allowed)
    })
    .await?;
    if !status.controlling {
        viewer.stop().await?;
        return Err("the host is sharing view-only".into());
    }
    let notices = status.notice.map_or(0, |notice| notice.seq);
    let mode = match viewer.set_display_mode(target) {
        Ok(mode) => mode,
        Err(failure) => {
            viewer.stop().await?;
            return Err(match failure {
                DisplayModeError::NotOffered => "the host offers no display modes",
                DisplayModeError::NotControlling => "control was lost before the change",
                DisplayModeError::NoFit => "no mode of the host's fits that size",
                DisplayModeError::NotListed => "the host does not list that mode",
                DisplayModeError::Closed => "the session ended",
                _ => "the change was refused",
            }
            .into());
        }
    };
    println!("asked for {}x{}", mode.width, mode.height);
    // The host answers with the new picture, or a notice when it refuses.
    let status = until(&viewer, "the host's answer", |status| {
        shows(status, mode)
            || status.notice.as_ref().is_some_and(|notice| {
                notice.seq > notices && notice.code == NoticeCode::DisplayModeFailed
            })
    })
    .await?;
    if !shows(&status, mode) {
        let notice = status
            .notice
            .map(|notice| notice.message)
            .unwrap_or_default();
        viewer.stop().await?;
        return Err(format!("the host refused: {notice}").into());
    }
    println!(
        "the host's display shows {}x{}; holding it {hold:?}",
        mode.width, mode.height
    );
    tokio::time::sleep(hold).await;
    viewer.stop().await?;

    // Leaving put it back: a new session finds the original mode.
    let viewer = connect(&client, &invitation).await?;
    let status = until(&viewer, "the host's display modes", |status| {
        status.modes.is_some()
    })
    .await?;
    viewer.stop().await?;
    let modes = status.modes.ok_or("the host offers no display modes")?;
    println!(
        "after the disconnect the host's display shows {}x{}{}",
        modes.current.width,
        modes.current.height,
        if modes.changed() {
            ", still changed"
        } else {
            ", its own mode"
        }
    );
    Ok(())
}

/// A viewer that decodes H.264 up to 4K and presents every unit, as a
/// renderer would once shown: control, and so mode changes, need a current
/// picture. The default resolution asks for the display's own size.
async fn connect(client: &Client, invitation: &Invitation) -> Outcome<Viewer> {
    let decoders = Decoders {
        h264: Some((3840, 2160)),
        h265: None,
    };
    let config = ViewerConfig::for_decoders(decoders, Resolution::default(), 60.0, None);
    let viewer = Viewer::connect(client, invitation, ConnectOptions::default(), config).await?;
    let consumer = viewer.video_sink().attach()?;
    std::thread::spawn(move || {
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
    Ok(viewer)
}

fn print_modes(modes: &ModeList) {
    println!(
        "current {}x{}, original {}x{}{}",
        modes.current.width,
        modes.current.height,
        modes.original.width,
        modes.original.height,
        if modes.changed() {
            " (changed by a viewer)"
        } else {
            ""
        }
    );
    let supported: Vec<String> = modes
        .supported
        .iter()
        .map(|mode| format!("{}x{}", mode.width, mode.height))
        .collect();
    println!("supported: {}", supported.join(", "));
}

/// The picture is current and its display at `mode`.
fn shows(status: &ViewerStatus, mode: Mode) -> bool {
    status.fresh
        && status
            .display
            .as_ref()
            .is_some_and(|display| (display.width, display.height) == (mode.width, mode.height))
}

fn size(text: &str) -> Outcome<Mode> {
    let (width, height) = text.split_once('x').ok_or("sizes read WxH, as 1920x1080")?;
    Ok(Mode {
        width: width.parse()?,
        height: height.parse()?,
    })
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
