//! The remote desktop end to end in one process, on any OS: a relay, a host
//! of a synthetic desktop, and a viewer that controls it.
//!
//! ```text
//! cargo run -p pb-mapper --features desktop --example desktop_loopback
//! ```
//!
//! The host's `Backend` is the example's own (`desktop_support`): it makes
//! small fake H.264 units and records the input it receives, so nothing
//! touches this computer's screen, keyboard, mouse or display modes. The
//! viewer takes a picture, takes control, moves, clicks and types, changes
//! the preset, pauses and resumes, fits the host's display to its own size
//! and sees the host put it back when it disconnects, then prints what it
//! saw. It exits 0 when every step did what it should.
//!
//! With `--serve` it only hosts, until Ctrl+C, and prints what another
//! process needs to view or control the synthetic desktop: run
//! `desktop_viewer`, `desktop_control` or `desktop_display_mode` against it.

mod desktop_support;

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use desktop_support::{LocalRelay, MODES, Panel, Synthetic};
use pb_mapper::ConnectOptions;
use pb_mapper::desktop::{
    Button, DesktopView, Host, HostOptions, HostState, Input, InputError, Mode, ModeTarget,
    QualityPreset, VideoCodec, Viewer, ViewerConfig, ViewerState, ViewerStatus,
};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::main]
async fn main() -> Outcome {
    let serve = std::env::args().any(|arg| arg == "--serve");
    let relay = LocalRelay::start("pb-mapper-desktop-loopback").await?;
    let client = relay.client()?;

    // 1. Host the synthetic desktop. `HostOptions::new` takes the tunnel
    //    defaults: LAN first, then direct, then the relay. Control is off
    //    unless the host allows it.
    let panel = Arc::new(Panel::default());
    let mut options = HostOptions::new("desktop:loopback");
    options.allow_control = true;
    let desktop = panel.clone();
    let host = Host::builder(options)
        .backend(move || Ok(Synthetic::new(desktop.clone(), false)))
        .start(&client)
        .await?;
    if serve {
        return serve_until_ctrl_c(relay, host, &panel).await;
    }
    let mut hosting = host.watch(Duration::from_millis(100));
    hosting.mark_changed();
    let host_log = tokio::spawn(async move {
        while hosting.changed().await.is_ok() {
            let status = hosting.borrow_and_update().clone();
            let viewer = status.viewer.map(|viewer| {
                format!(
                    ", viewer on {:?}, controlling {}, {:?}",
                    viewer.path, viewer.controlling, viewer.quality
                )
            });
            println!("  host: {:?}{}", status.state, viewer.unwrap_or_default());
        }
    });

    // 2. View it, offering the codec this renderer takes.
    let viewer = Viewer::connect(
        &client,
        host.invitation(),
        ConnectOptions::default(),
        ViewerConfig::new(vec![VideoCodec::H264]),
    )
    .await?;
    let renderer = desktop_support::render(&viewer, |_| true);
    let status = until(&viewer, "the first picture", |status| status.fresh).await?;
    let display = status.display.ok_or("no display")?;
    println!(
        "viewing {}x{} as {:?}, path {}",
        display.width, display.height, status.stats.codec, status.stats.path
    );

    // 3. Take control, then move, click and type: input is in the host
    //    display's pixels, and text goes in chunks the host accepts.
    viewer.request_control(true)?;
    until(&viewer, "control", |status| status.controlling).await?;
    send(&viewer, |view| Some(vec![view.display_point(0.25, 0.75)?])).await?;
    let click = [true, false].map(|down| Input::Button {
        button: Button::Left,
        down,
        drag_start: false,
    });
    send(&viewer, |_| Some(click.to_vec())).await?;
    let text = "Hello from the loopback desktop. ".repeat(40);
    let chunks: Vec<Input> = Input::text_chunks(&text).collect();
    for chunk in &chunks {
        send(&viewer, |_| Some(vec![chunk.clone()])).await?;
    }
    let sent = 1 + click.len() + chunks.len();
    let started = Instant::now();
    let inputs = loop {
        let inputs = panel.inputs();
        if inputs.len() >= sent {
            break inputs;
        }
        if started.elapsed() > Duration::from_secs(10) {
            return Err(format!("the host applied {} of {sent} inputs", inputs.len()).into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let Input::Move { x, y } = inputs[0] else {
        return Err("the first input applied was not the move".into());
    };
    let typed: String = inputs
        .iter()
        .filter_map(|input| match input {
            Input::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if (x, y) != (display.width / 4, display.height * 3 / 4) || typed != text {
        return Err(format!(
            "the host applied a move to {x},{y} and {} typed bytes",
            typed.len()
        )
        .into());
    }
    println!(
        "the host applied a move to {x},{y}, a click and {} bytes of text in {} chunks",
        typed.len(),
        chunks.len()
    );

    // 4. The preset changes live, without reconnecting.
    viewer.set_quality(QualityPreset::Smooth);
    until_host(&host, "the smooth preset", |status| {
        status
            .viewer
            .as_ref()
            .is_some_and(|viewer| viewer.quality == QualityPreset::Smooth)
    })
    .await?;

    // 5. Hidden: the host stops capturing until the viewer resumes.
    viewer.pause();
    until_host(&host, "the host to pause", |status| {
        status.state == HostState::Paused
    })
    .await?;
    viewer.resume();
    until(&viewer, "a picture after resuming", |status| {
        status.state == ViewerState::Streaming && status.fresh
    })
    .await?;

    // 6. Fit the host's display to this viewer's, as a desktop app's "Fit
    //    local" does. A mode change needs control, which the pause ended.
    viewer.request_control(true)?;
    let status = until(&viewer, "control and the host's modes", |status| {
        status.controlling && status.modes.is_some()
    })
    .await?;
    let modes = status.modes.ok_or("the host offers no display modes")?;
    let local = Mode {
        width: 1024,
        height: 576,
    };
    let fitted = viewer
        .set_display_mode(ModeTarget::FitLocal(local))
        .map_err(|failure| format!("no mode change: {failure:?}"))?;
    until(&viewer, "the picture at the fitted size", |status| {
        status.fresh
            && status.display.as_ref().is_some_and(|display| {
                (display.width, display.height) == (fitted.width, fitted.height)
            })
    })
    .await?;
    if panel.mode() != fitted {
        return Err("the backend's display is not at the fitted mode".into());
    }
    println!(
        "fitted the host's {}x{} display to {}x{} for this viewer",
        modes.original.width, modes.original.height, fitted.width, fitted.height
    );

    // 7. What the session measured over two seconds, and the tunnel under it.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let status = viewer.status();
    let stats = &status.stats;
    println!(
        "presented {:.0} fps, received {:.0} fps, {} keyframes, rtt {:?} ms, {} kbit/s",
        stats.presented_fps,
        stats.received_fps,
        stats.keyframes,
        stats.rtt_ms,
        stats.received_bitrate / 1000
    );
    if let Some(metrics) = viewer.metrics() {
        println!(
            "viewer tunnel: {} direct and {} relay sessions, {} bytes received direct",
            metrics.sessions.direct, metrics.sessions.relay, metrics.traffic.direct.received
        );
    }

    // 8. Disconnect: the host puts its display back as the session ends.
    viewer.release_input()?;
    viewer.stop().await?;
    let started = Instant::now();
    while panel.mode() != MODES[0] {
        if started.elapsed() > Duration::from_secs(10) {
            return Err("the host did not put its display back".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "the host put its display back to {}x{} when the viewer left",
        MODES[0].width, MODES[0].height
    );
    let presented = renderer.join().map_err(|_| "the renderer panicked")?;
    host.stop().await?;
    host_log.await?;
    relay.stop();
    println!("desktop_loopback: ok, {presented} frames presented");
    Ok(())
}

/// Queue the batch `batch` makes of the current view, under that view's
/// generation, so its points belong to the display they were taken from.
/// While no picture of a generation is current yet, the batch is made again
/// from a newer view.
async fn send(viewer: &Viewer, batch: impl Fn(&DesktopView) -> Option<Vec<Input>>) -> Outcome {
    let started = Instant::now();
    loop {
        let view = viewer.view();
        let inputs = batch(&view).ok_or("no display to point at")?;
        match viewer.input(view.generation, &inputs) {
            Ok(()) => return Ok(()),
            Err(InputError::NotFresh) if started.elapsed() < Duration::from_secs(5) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(failure) => return Err(failure.into()),
        }
    }
}

/// The viewer's status once `done` holds, within 10 s.
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
        if status.state == ViewerState::Stopped || started.elapsed() > Duration::from_secs(10) {
            return Err(format!(
                "no {what}: {:?} {:?} {}",
                status.state, status.reason, status.message
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn until_host(
    host: &Host,
    what: &str,
    done: impl Fn(&pb_mapper::desktop::HostStatus) -> bool,
) -> Outcome {
    let started = Instant::now();
    while !done(&host.status()) {
        if started.elapsed() > Duration::from_secs(10) {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Host until Ctrl+C, printing what reaches the synthetic desktop.
async fn serve_until_ctrl_c(relay: LocalRelay, host: Host, panel: &Panel) -> Outcome {
    println!("Hosting a synthetic desktop. In another terminal, with:");
    println!("  PB_MAPPER_SERVER={}", relay.addr);
    println!("  MSG_HEADER_KEY={}", relay.key);
    println!(
        "  PB_MAPPER_DESKTOP_INVITATION={}",
        host.invitation().encode()
    );
    println!("run desktop_viewer, desktop_control or desktop_display_mode. Ctrl+C stops.");
    let (mut seen, mut mode) = (0, panel.mode());
    let mut ticks = tokio::time::interval(Duration::from_millis(200));
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; break; }
            _ = ticks.tick() => {
                let inputs = panel.inputs();
                for input in &inputs[seen..] {
                    println!("  input: {input:?}");
                }
                seen = inputs.len();
                if panel.mode() != mode {
                    mode = panel.mode();
                    println!("  display mode: {}x{}", mode.width, mode.height);
                }
            }
        }
    }
    host.stop().await?;
    relay.stop();
    Ok(())
}
