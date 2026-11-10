//! The SDK script: a host of the guarded backend, a viewer that presents
//! what it receives, and the input it sends.
use crate::fixture::{Fixture, State, TEXT, failure};
use crate::guarded::Guarded;
use pb_mapper_client::sdk::{Client, ClientConfig, ConnectOptions, PathPolicy, RelayTransport};
use pb_mapper_desktop::{
    Button, DesktopView, Display, DisplaySelection, Host, HostOptions, Input, Pull, QualityPreset,
    Result, VideoCodec, VideoLimits, Viewer, ViewerConfig,
};
use pb_mapper_testkit::Relay;
use std::{
    sync::atomic::Ordering,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
/// The host holds a drag-start press until the double-click time after
/// the last click plus 20 ms, but never 600 ms past its arrival
/// (`DRAG_GUARD_*` in the worker): it always waits out up to this much.
const DRAG_HOLD_COVERS_MS: u32 = 580;
#[derive(serde::Deserialize)]
struct Configuration {
    server_address: String,
    msg_header_key: String,
    #[serde(default)]
    relay_transport: RelayTransport,
}
async fn wait_for(state: &State, what: &str, predicate: impl Fn() -> bool) -> Result<()> {
    let start = Instant::now();
    while !predicate() {
        state.check()?;
        if let Some(error) = state.input_error.lock().as_ref() {
            return Err(failure(error));
        }
        if start.elapsed() > Duration::from_secs(3) {
            return Err(failure(format!(
                "timed out waiting for {what}; applied={} owned_edit={:?}",
                state.applied.load(Ordering::Acquire),
                state.text()
            )));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    state.check()
}
/// As the app's input core sends: with the generation of the view it
/// drew. The viewer refuses it unless that generation's newest frame was
/// presented recently and control is granted, and stamps that frame and
/// the sequence itself.
fn send(viewer: &Viewer, state: &State, input: Input) -> Result<()> {
    state.check()?;
    viewer
        .input(viewer.view().generation, &[input])
        .map_err(failure)
}
/// The view's input gate: control, and a fresh presented frame.
fn live(viewer: &Viewer) -> bool {
    const LIVE: u8 = DesktopView::FLAG_CONTROLLING | DesktopView::FLAG_FRESH;
    viewer.view().flags & LIVE == LIVE
}
/// The native renderer's part: present each unit as it arrives, like a
/// decoder that never falls behind, until the session's video ends. Only
/// these presentations authorise input.
fn present(viewer: &Viewer) -> Result<JoinHandle<()>> {
    let consumer = viewer.video_sink().attach().map_err(failure)?;
    Ok(std::thread::spawn(move || {
        let mut capacity = 1 << 20;
        loop {
            match consumer.next(Duration::from_millis(100), capacity) {
                Pull::Unit(unit) => {
                    consumer.presented(unit.generation, unit.frame_id);
                }
                Pull::NeedCapacity(needed) => capacity = needed,
                Pull::Timeout | Pull::Paused => {}
                Pull::Closed => break,
            }
        }
    }))
}
/// Text, a click and a drag-start press on the owned window, then the
/// release of that press when the viewer disconnects.
async fn exercise(
    viewer: &Viewer,
    host: &Host,
    state: &State,
    display: &Display,
    topology: &str,
) -> Result<()> {
    wait_for(state, "first presented frame", || {
        viewer.view().presented_frame_id != 0
    })
    .await?;
    viewer.request_control(true)?;
    // A grant starts a new generation: input waits for its first
    // presented frame.
    wait_for(state, "control with a fresh presented frame", || {
        live(viewer)
    })
    .await?;
    send(viewer, state, Input::Text { text: TEXT.into() })?;
    if let Err(e) = wait_for(state, "Unicode text in owned EDIT", || state.text() == TEXT).await {
        eprintln!(
            "native input diagnostics: last_capture_ms={} viewer_state={:?} host_state={:?}",
            state.capture_ms.load(Ordering::Acquire),
            viewer.status(),
            host.status()
        );
        return Err(e);
    }
    let point = state.client_point(320, 220)?;
    if point.x < display.x
        || point.y < display.y
        || point.x >= display.x + display.width as i32
        || point.y >= display.y + display.height as i32
    {
        return Err(failure("owned target outside selected physical monitor"));
    }
    let applied = state.applied.load(Ordering::Acquire);
    let (x, y) = ((point.x - display.x) as u32, (point.y - display.y) as u32);
    send(viewer, state, Input::Move { x, y })?;
    wait_for(state, "native pointer move", || {
        state.applied.load(Ordering::Acquire) > applied
    })
    .await?;
    let left = |down, drag_start| Input::Button {
        button: Button::Left,
        down,
        drag_start,
    };
    send(viewer, state, left(true, false))?;
    wait_for(state, "native button down", || {
        state.downs.load(Ordering::Acquire) == 1 && state.held.load(Ordering::Acquire)
    })
    .await?;
    send(viewer, state, left(false, false))?;
    wait_for(state, "native button up", || {
        state.ups.load(Ordering::Acquire) == 1 && !state.held.load(Ordering::Acquire)
    })
    .await?;
    // A drag starting where the click just was: the host holds the press
    // past the double-click time, so Windows sees a new press rather than
    // a double click.
    send(viewer, state, left(true, true))?;
    wait_for(state, "held drag-start press before disconnect", || {
        state.downs.load(Ordering::Acquire) == 2 && state.held.load(Ordering::Acquire)
    })
    .await?;
    let double_click_ms = unsafe { GetDoubleClickTime() };
    let (double_clicks, press_gap_ms) = (
        state.double_clicks.load(Ordering::Acquire),
        state.press_gap_ms.load(Ordering::Acquire),
    );
    if double_clicks > 0 && double_click_ms <= DRAG_HOLD_COVERS_MS {
        return Err(failure(format!(
            "the drag-start press merged into a double click: press_gap_ms={press_gap_ms} double_click_ms={double_click_ms}"
        )));
    }
    let at = Instant::now();
    viewer.stop().await?;
    wait_for(state, "disconnect release", || {
        state.ups.load(Ordering::Acquire) == 2 && !state.held.load(Ordering::Acquire)
    })
    .await?;
    if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
        return Err(failure("native left button remains held after disconnect"));
    }
    println!(
        "native_input_acceptance path={topology} unicode_verified=true button_down={} button_up={} drag_press_gap_ms={press_gap_ms} double_click_ms={double_click_ms} double_clicks={double_clicks} disconnect_release_ms={:.3} physical_display={}x{}",
        state.downs.load(Ordering::Acquire),
        state.ups.load(Ordering::Acquire),
        at.elapsed().as_secs_f64() * 1000.0,
        display.width,
        display.height
    );
    Ok(())
}
pub fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let display = pb_mapper_desktop::displays()?
        .into_iter()
        .next()
        .ok_or_else(|| failure("no ordinary display"))?;
    if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
        return Err(failure("physical left button already held; refusing test").into());
    }
    let fixture = Fixture::open(&display)?;
    let state = fixture.state.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let args: Vec<_> = std::env::args().collect();
        let value = |name| {
            args.windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1].as_str())
        };
        for option in ["--relay-config", "--consumer-server"] {
            if args.iter().any(|arg| arg == option) && value(option).is_none() {
                return Err(failure(format!("{option} requires a value")));
            }
        }
        let local_relay = if value("--relay-config").is_none() {
            Some(Relay::start("desktop-owned-native-input").await)
        } else {
            None
        };
        let (server, transport, admin_client) = if let Some(relay) = &local_relay {
            let server = relay.addr().to_string();
            let client = Client::from_credential(
                server.clone(),
                pb_mapper_testkit::admin_credential(),
                true,
                None,
            );
            (server, RelayTransport::Tcp, client)
        } else {
            let config = serde_json::from_slice::<Configuration>(&std::fs::read(
                value("--relay-config").ok_or_else(|| failure("relay config required"))?,
            )?)
            .map_err(failure)?;
            let transport = if args.iter().any(|arg| arg == "--tcp") {
                RelayTransport::Tcp
            } else {
                config.relay_transport
            };
            let client = Client::new(ClientConfig {
                server: config.server_address.clone(),
                credential: config.msg_header_key,
                keep_alive: true,
                namespace: None,
            })
            .map_err(failure)?
            .with_relay_transport(transport)
            .map_err(failure)?;
            (config.server_address, transport, client)
        };
        let admin = admin_client.admin().map_err(failure)?;
        let issued = admin
            .issue_key(Duration::from_secs(120), Some("owned-native-input".into()))
            .await
            .map_err(failure)?;
        let result = async {
            let client = Client::new(ClientConfig {
                server: server.clone(),
                credential: issued.credential.clone(),
                keep_alive: true,
                namespace: None,
            })
            .map_err(failure)?
            .with_relay_transport(transport)
            .map_err(failure)?;
            let consumer = Client::new(ClientConfig {
                server: value("--consumer-server").unwrap_or(&server).to_string(),
                credential: issued.credential.clone(),
                keep_alive: true,
                namespace: None,
            })
            .map_err(failure)?
            .with_relay_transport(transport)
            .map_err(failure)?;
            let topology = if value("--consumer-server").is_some() {
                "configured_separate_relay_entries"
            } else if local_relay.is_some() {
                "local_tcp_relay"
            } else {
                "deployed_relay_same_machine_endpoints"
            };
            let backend_state = state.clone();
            let id = display.id;
            let mut options = HostOptions::new("desktop:owned-input-acceptance");
            options.display = DisplaySelection::Id(id);
            options.allow_control = true;
            options.allow_direct = false;
            let host = Host::builder(options)
                .backend(move || {
                    Ok(Guarded {
                        inner: pb_mapper_desktop::native_backend(&DisplaySelection::Id(id))?,
                        state: backend_state.clone(),
                    })
                })
                .start(&client)
                .await?;
            // Use the product's native GPU path with a small input-acceptance
            // workload. Debug JPEG resizing can hold the capture thread for a
            // second per frame, and input waits behind it.
            let config = ViewerConfig {
                codecs: vec![VideoCodec::H264],
                limits: VideoLimits {
                    max_width: 1280,
                    max_height: 720,
                    max_fps: 30,
                },
                fps_override: false,
                quality: QualityPreset::Balanced,
                progress: None,
            };
            let connected = Viewer::connect(
                &consumer,
                host.invitation(),
                ConnectOptions {
                    path: PathPolicy::RelayOnly,
                    ..Default::default()
                },
                config,
            )
            .await;
            let viewer = match connected {
                Ok(viewer) => viewer,
                Err(error) => {
                    let _ = host.stop().await;
                    return Err(failure(error));
                }
            };
            let presenter = present(&viewer);
            let result = match &presenter {
                Ok(_) => exercise(&viewer, &host, &state, &display, topology).await,
                Err(error) => Err(failure(error)),
            };
            let _ = viewer.stop().await;
            // The stop closed the video sink, which ends the presenting thread.
            if let Ok(presenter) = presenter {
                let _ = presenter.join();
            }
            let stopped = host.stop().await;
            result?;
            stopped
        }
        .await;
        let revoked = admin.revoke_key(issued.key_id).await;
        result?;
        revoked.map(|_| ()).map_err(failure)
    })?;
    drop(fixture);
    Ok(())
}
