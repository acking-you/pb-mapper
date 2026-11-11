//! The SDK script: a host of the guarded backend, a viewer that presents
//! what it receives, and the input it sends.
use crate::fixture::{CUSTOM_HOTSPOT, Fixture, LINES, State, TEXT, failure, modifier_held};
use crate::guarded::Guarded;
use crate::probe_support::{RelayConfig, option, relay_client};
use pb_mapper_client::sdk::{Client, ConnectOptions, PathPolicy, RelayTransport};
use pb_mapper_desktop::{
    Button, CursorKind, CursorShape, DesktopView, Display, DisplaySelection, Host, HostOptions,
    Input, Pull, QualityPreset, Result, VideoCodec, VideoLimits, Viewer, ViewerConfig,
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
    send_all(viewer, state, &[input])
}
fn send_all(viewer: &Viewer, state: &State, inputs: &[Input]) -> Result<()> {
    state.check()?;
    viewer
        .input(viewer.view().generation, inputs)
        .map_err(failure)
}
fn physical(usage: u16, down: bool) -> Input {
    Input::PhysicalKey { usage, down }
}
/// A whole tap of the key at keyboard-page `usage`.
fn tap(usage: u16) -> [Input; 2] {
    [physical(usage, true), physical(usage, false)]
}
const SHIFT_LEFT: u16 = 0xE1;
/// Text with a line break and a tab, then physical keys: what this window's
/// layout types with its input method off, and what an input method on it
/// composes from them, which is only reported.
async fn keyboard(viewer: &Viewer, state: &State) -> Result<String> {
    state.clear()?;
    send(viewer, state, Input::Text { text: LINES.into() })?;
    wait_for(state, "one Enter and one Tab in the owned EDIT", || {
        state.text() == "a\r\nb\tc"
    })
    .await?;

    // h, i and Shift+1, by place: set-1 scancodes 0x23, 0x17 and 0x02.
    state.clear()?;
    state.ime(false)?;
    let keys = [
        tap(0x0B).as_slice(),
        &tap(0x0C),
        &[physical(SHIFT_LEFT, true)],
        &tap(0x1E),
        &[physical(SHIFT_LEFT, false)],
    ]
    .concat();
    send_all(viewer, state, &keys)?;
    let expected = [(0x23, false), (0x17, false), (0x02, true)]
        .map(|(scan, shift)| state.typed_by_layout(scan, shift))
        .concat();
    wait_for(state, "physical keys typed by the layout", || {
        state.text() == expected
    })
    .await?;
    wait_for(state, "physical Shift released", || !left_shift_down()).await?;

    // n, i with this window's input method open: reported, then cancelled.
    state.clear()?;
    state.ime(true)?;
    let mut composed = String::new();
    if state.ime_open() {
        let keys = [tap(0x11), tap(0x0C)].concat();
        let sent = send_all(viewer, state, &keys);
        let start = Instant::now();
        while sent.is_ok() && start.elapsed() < Duration::from_secs(1) {
            composed = state.composing();
            if composed.chars().count() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        state.ime_restore();
        sent?;
    }
    Ok(format!(
        "lines_verified=true physical_typed={expected:?} ime_composition={composed:?} ime_edit_text={:?}",
        state.text()
    ))
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
/// The pointer's shape as the viewer sees it, over the owned window: the
/// class arrow, the EDIT's text beam, a resize arrow and this test's own
/// monochrome cursor, which must arrive pixel for pixel. One line per shape
/// for the record; with `--dump-cursors DIR`, each as a JPEG over grey, 8×.
async fn shapes(viewer: &Viewer, state: &State, display: &Display) -> Result<String> {
    let mut lines = Vec::new();
    for (name, (x, y), kind) in [
        ("client", (320, 120), CursorKind::Arrow),
        ("edit", (320, 66), CursorKind::Ibeam),
        ("resize_box", (600, 270), CursorKind::ResizeNwse),
        ("custom_box", (64, 270), CursorKind::Custom),
    ] {
        let point = state.client_point(x, y)?;
        let (x, y) = ((point.x - display.x) as u32, (point.y - display.y) as u32);
        send(viewer, state, Input::Move { x, y })?;
        wait_for(
            state,
            &format!("the {name} pointer shape at the viewer"),
            || shape_of(viewer).is_some_and(|shape| shape.kind() == kind),
        )
        .await?;
        let shape = shape_of(viewer).ok_or_else(|| failure("the shape went away"))?;
        if kind == CursorKind::Custom {
            check_custom(&shape)?;
        }
        let pixels = shape.rgba().as_chunks::<4>().0;
        let (opaque, clear) = (
            pixels.iter().filter(|p| p[3] == 255).count(),
            pixels.iter().filter(|p| p[3] == 0).count(),
        );
        // Straight alpha: a colour channel above its pixel's alpha.
        let straight = pixels.iter().any(|p| p[..3].iter().any(|c| *c > p[3]));
        lines.push(format!(
            "cursor_shape over={name} kind={:?} size={}x{} hotspot={:?} opaque={opaque} clear={clear} partial={} straight_alpha_seen={straight}",
            shape.kind(),
            shape.width(),
            shape.height(),
            shape.hotspot(),
            pixels.len() - opaque - clear,
        ));
        if let Some(dir) = option(&std::env::args().collect::<Vec<_>>(), "--dump-cursors") {
            dump(
                &shape,
                &std::path::Path::new(dir).join(format!("{name}.jpg")),
            )?;
        }
    }
    Ok(lines.join("\n"))
}

fn shape_of(viewer: &Viewer) -> Option<CursorShape> {
    viewer.cursor_shape(viewer.view().cursor_shape)
}

/// The custom cursor as `fixture::custom_planes` drew it, at its own size.
fn check_custom(shape: &CursorShape) -> Result<()> {
    let at = |x: u32, y: u32| {
        let i = ((y * shape.width() + x) * 4) as usize;
        shape.rgba().get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
    };
    let expected = [
        ((2, 5), [0, 0, 0, 255]),
        ((20, 5), [255; 4]),
        ((12, 20), [0, 0, 0, 255]),
        ((7, 20), [255; 4]),
        ((12, 21), [255; 4]),
        ((30, 30), [0; 4]),
    ];
    let size = (shape.width(), shape.height(), shape.hotspot());
    if size != (32, 32, CUSTOM_HOTSPOT)
        || expected
            .iter()
            .any(|((x, y), pixel)| at(*x, *y) != Some(*pixel))
    {
        return Err(failure(format!(
            "the custom cursor arrived as {size:?} with {:?}",
            expected.map(|((x, y), _)| at(x, y))
        )));
    }
    Ok(())
}

/// `shape` over mid grey, eight times its size, as a JPEG at `path`.
fn dump(shape: &CursorShape, path: &std::path::Path) -> Result<()> {
    const SCALE: u32 = 8;
    let (width, height) = (shape.width() * SCALE, shape.height() * SCALE);
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let i = (((y / SCALE) * shape.width() + x / SCALE) * 4) as usize;
            let p = &shape.rgba()[i..i + 4];
            let over = |c: u8| {
                ((u32::from(c) * u32::from(p[3]) + 128 * (255 - u32::from(p[3]))) / 255) as u8
            };
            rgb.extend([over(p[0]), over(p[1]), over(p[2])]);
        }
    }
    let file = std::fs::File::create(path)?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(failure)
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
    let keyboard = keyboard(viewer, state).await?;
    let shapes = shapes(viewer, state, display).await?;
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
    // A physical key held too: the host releases it with the press.
    send(viewer, state, physical(SHIFT_LEFT, true))?;
    wait_for(
        state,
        "physical Shift held before disconnect",
        left_shift_down,
    )
    .await?;
    let at = Instant::now();
    viewer.stop().await?;
    let released = wait_for(state, "disconnect release", || {
        state.ups.load(Ordering::Acquire) == 2
            && !state.held.load(Ordering::Acquire)
            && !left_shift_down()
    })
    .await;
    if left_shift_down() {
        // Never leave this computer's Shift down, whatever failed.
        release_left_shift();
        return Err(failure("physical Shift remains held after disconnect"));
    }
    released?;
    if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
        return Err(failure("native left button remains held after disconnect"));
    }
    println!("{shapes}");
    println!(
        "native_input_acceptance path={topology} unicode_verified=true {keyboard} button_down={} button_up={} drag_press_gap_ms={press_gap_ms} double_click_ms={double_click_ms} double_clicks={double_clicks} disconnect_release_ms={:.3} physical_display={}x{}",
        state.downs.load(Ordering::Acquire),
        state.ups.load(Ordering::Acquire),
        at.elapsed().as_secs_f64() * 1000.0,
        display.width,
        display.height
    );
    Ok(())
}
fn left_shift_down() -> bool {
    unsafe { GetAsyncKeyState(i32::from(VK_LSHIFT.0)) < 0 }
}
/// The left Shift up, by scancode, as the cleanup of a failed release.
fn release_left_shift() {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wScan: 0x2a,
                dwFlags: KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP,
                ..Default::default()
            },
        },
    };
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}
pub fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let display = pb_mapper_desktop::displays()?
        .into_iter()
        .next()
        .ok_or_else(|| failure("no ordinary display"))?;
    if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
        return Err(failure("physical left button already held; refusing test").into());
    }
    if modifier_held() {
        return Err(failure("a modifier key is held; refusing test").into());
    }
    let fixture = Fixture::open(&display)?;
    let state = fixture.state.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let args: Vec<_> = std::env::args().collect();
        for name in ["--relay-config", "--consumer-server"] {
            if args.iter().any(|arg| arg == name) && option(&args, name).is_none() {
                return Err(failure(format!("{name} requires a value")));
            }
        }
        let local_relay = if option(&args, "--relay-config").is_none() {
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
            let config = serde_json::from_slice::<RelayConfig>(&std::fs::read(
                option(&args, "--relay-config").ok_or_else(|| failure("relay config required"))?,
            )?)
            .map_err(failure)?;
            let transport = if args.iter().any(|arg| arg == "--tcp") {
                RelayTransport::Tcp
            } else {
                config.relay_transport
            };
            let client = relay_client(
                config.server_address.clone(),
                config.msg_header_key,
                transport,
            )
            .map_err(failure)?;
            (config.server_address, transport, client)
        };
        let admin = admin_client.admin().map_err(failure)?;
        let issued = admin
            .issue_key(Duration::from_secs(120), Some("owned-native-input".into()))
            .await
            .map_err(failure)?;
        let result = async {
            let client = relay_client(server.clone(), issued.credential.clone(), transport)
                .map_err(failure)?;
            let consumer = relay_client(
                option(&args, "--consumer-server")
                    .unwrap_or(&server)
                    .to_string(),
                issued.credential.clone(),
                transport,
            )
            .map_err(failure)?;
            let topology = if option(&args, "--consumer-server").is_some() {
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
