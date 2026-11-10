use super::background::*;
use super::host::*;
use super::viewer::*;
use super::*;
use crate::error::ErrorCode;
use base64::Engine as _;
use pb_mapper::desktop::{
    ConnectStage, DesktopView, QualityPreset, Resolution, VideoCodec, VideoLimits, Viewer,
    ViewerConfig,
};
use std::{
    ffi::{CStr, CString},
    future::Future,
    sync::atomic::Ordering,
    task::{Context, Waker},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

fn test_handle(name: &str) -> PbMapperHandle {
    let root = std::env::temp_dir().join(format!("pb-desktop-{name}-{}", std::process::id()));
    // A new state writes the process credential, which a live relay's
    // framing reads, so it is made under the lock such tests hold.
    let _credential_guard =
        pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
    PbMapperHandle {
        desktop: Default::default(),
        tunnel_events: Default::default(),
        state: Arc::new(tokio::sync::Mutex::new(crate::state::PbMapperState::new(
            Some(root.display().to_string()),
        ))),
        control_shutdown: Default::default(),
        runtime: tokio::runtime::Runtime::new().unwrap(),
    }
}

fn reply(text: *mut c_char) -> Value {
    let value = serde_json::from_slice(unsafe { CStr::from_ptr(text) }.to_bytes()).unwrap();
    unsafe { crate::logging::pb_mapper_free_string(text) };
    value
}

/// The lifecycle export, as a Dart worker calls it.
fn call_json(handle: &PbMapperHandle, request: Value) -> Value {
    let request = CString::new(request.to_string()).unwrap();
    reply(unsafe {
        pb_mapper_desktop_call_json(std::ptr::from_ref(handle).cast_mut(), request.as_ptr())
    })
}

/// The hot export, as the UI isolate calls it.
fn hot(handle: &PbMapperHandle, request: Value) -> Value {
    let request = CString::new(request.to_string()).unwrap();
    reply(unsafe {
        pb_mapper_desktop_viewer_call_json(std::ptr::from_ref(handle).cast_mut(), request.as_ptr())
    })
}

fn view(handle: &PbMapperHandle) -> (i32, PbDesktopViewV1) {
    let mut view = PbDesktopViewV1 {
        generation: 9,
        ..Default::default()
    };
    let code =
        unsafe { pb_mapper_desktop_view_v1(std::ptr::from_ref(handle).cast_mut(), &mut view) };
    (code, view)
}

fn invitation() -> Invitation {
    desktop::HostIdentity::generate()
        .unwrap()
        .invitation("desktop:test".into())
        .unwrap()
}

fn connected() -> Connected {
    Connected {
        invitation: invitation(),
        relay: "relay:7666".into(),
        namespace: 0,
    }
}

/// One host record: a `u32` big-endian length, then JSON.
async fn send(host: &mut DuplexStream, message: Value) {
    let bytes = serde_json::to_vec(&message).unwrap();
    host.write_u32(bytes.len() as u32).await.unwrap();
    host.write_all(&bytes).await.unwrap();
}

/// A viewer whose scripted host accepted at once and streams the first
/// generation. The host side stays open and silent, so `stop()` waits out
/// the viewer's close grace, seconds long: a held stop.
async fn scripted_viewer() -> (Viewer, DuplexStream) {
    let (stream, mut host) = tokio::io::duplex(1 << 20);
    let connecting = tokio::spawn(Viewer::over_stream_for_test(
        stream,
        "t".repeat(43),
        ViewerConfig {
            codecs: vec![VideoCodec::H264],
            limits: VideoLimits::for_viewer(60.0),
            fps_override: false,
            quality: QualityPreset::Balanced,
            progress: None,
        },
    ));
    let authenticate = host.read_u32().await.unwrap();
    host.read_exact(&mut vec![0; authenticate as usize])
        .await
        .unwrap();
    send(
        &mut host,
        json!({"t": "accepted", "codec": "h264", "nominal_fps": 60, "quality": "balanced",
            "limits": {"max_width": 1920, "max_height": 1080, "max_fps": 60}, "capabilities": []}),
    )
    .await;
    send(
        &mut host,
        json!({"t": "state", "state": "streaming", "message": "", "control_allowed": true,
            "controlling": false, "generation": 1, "desktop": "ordinary",
            "display": {"id": 1, "name": "scripted", "x": 0, "y": 0, "width": 1920, "height": 1080},
            "video": {"width": 1920, "height": 1080}}),
    )
    .await;
    (connecting.await.unwrap().unwrap(), host)
}

fn eventually(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn hot_ops_never_wait_for_a_slow_disconnect() {
    let handle = test_handle("hot-ops");
    let handle = &handle;
    let _host = handle.runtime.block_on(async {
        let (viewer, host) = scripted_viewer().await;
        let (open, opened) = tokio::sync::oneshot::channel::<()>();
        let connecting = handle.desktop.connect_viewer(|_| async {
            opened.await.unwrap();
            Ok((viewer, connected()))
        });
        // Hidden while connecting: the session starts paused.
        let hiding = async {
            let status = hot(handle, json!({"op": "viewer_status"}));
            assert_eq!(
                status["data"],
                json!({"state": "connecting", "stage": null})
            );
            assert_eq!(hot(handle, json!({"op": "pause"}))["success"], true);
            open.send(()).unwrap();
        };
        let (connected, ()) = tokio::join!(connecting, hiding);
        connected.unwrap();
        host
    });
    assert_eq!(view(handle).1.state, DesktopView::STATE_PAUSED);
    assert_eq!(hot(handle, json!({"op": "resume"}))["success"], true);

    // Live: every op reaches the viewer.
    eventually(|| view(handle).1.state == DesktopView::STATE_STREAMING);
    let (code, live) = view(handle);
    assert_eq!(code, 1);
    assert_eq!(
        (live.generation, live.display_width, live.video_height),
        (1, 1920, 1080)
    );
    assert_eq!(
        hot(handle, json!({"op": "viewer_status"}))["data"]["state"],
        "streaming"
    );
    let input =
        json!({"op": "input", "generation": 1, "inputs": [{"kind": "move", "x": 1, "y": 1}]});
    assert_eq!(hot(handle, input.clone())["code"], "NOT_FRESH");
    // This host offers no display modes, and says nothing of its OS.
    let fit =
        json!({"op": "display_mode", "target": {"fit_local": {"width": 1280, "height": 720}}});
    assert_eq!(hot(handle, fit.clone())["code"], "DISPLAY_MODE_NOT_OFFERED");
    let local = json!({"op": "viewer_status", "local": {"width": 1280, "height": 720}});
    let status = &hot(handle, local)["data"];
    assert!(status.get("modes").is_none() && status["host_os"].is_null());
    for request in [
        json!({"op": "control", "enabled": true}),
        json!({"op": "keyframe"}),
        json!({"op": "quality", "preset": "smooth"}),
        json!({"op": "pause"}),
        json!({"op": "resume"}),
        json!({"op": "release"}),
        json!({"op": "control", "enabled": false}),
    ] {
        assert_eq!(hot(handle, request.clone())["success"], true, "{request}");
    }
    assert_eq!(
        hot(
            handle,
            json!({"op": "presented", "generation": 1, "frame_id": 1})
        )["code"],
        "INVALID_ARGUMENT"
    );
    assert_eq!(
        unsafe { pb_mapper_desktop_view_v1(std::ptr::null_mut(), &mut Default::default()) },
        -1
    );
    assert_eq!(
        unsafe {
            pb_mapper_desktop_view_v1(std::ptr::from_ref(handle).cast_mut(), std::ptr::null_mut())
        },
        -1
    );

    let source = handle.desktop.viewer.read().as_ref().unwrap().source;
    let consumer = desktop_video::attach(source);
    assert!(consumer > 0);
    std::thread::scope(|scope| {
        let pulling = scope.spawn(move || {
            let mut meta = desktop_video::PbDesktopVideoMetaV1::default();
            let mut buffer = [0u8; 16];
            let code = unsafe {
                desktop_video::pb_mapper_desktop_video_next_v1(
                    consumer as u64,
                    1000,
                    buffer.as_mut_ptr(),
                    16,
                    &mut meta,
                )
            };
            (code, Instant::now())
        });
        std::thread::sleep(Duration::from_millis(50));
        let disconnected_at = Instant::now();
        let disconnecting = scope.spawn(|| call_json(handle, json!({"op": "disconnect"})));
        // The source closes before the viewer's stop starts.
        let (code, returned) = pulling.join().unwrap();
        assert_eq!(code, desktop_video::CLOSED);
        assert!(returned.saturating_duration_since(disconnected_at) < Duration::from_millis(100));

        // Inside the runtime, where entering it again would panic.
        handle.runtime.block_on(async {
            for (request, expected) in [
                (
                    json!({"op": "viewer_status"}),
                    json!({"success": true, "data": null}),
                ),
                (
                    json!({"op": "control", "enabled": false}),
                    json!({"success": true, "data": {}}),
                ),
                (
                    json!({"op": "release"}),
                    json!({"success": true, "data": {}}),
                ),
                (
                    json!({"op": "keyframe"}),
                    json!({"success": true, "data": {"requested": false}}),
                ),
                (
                    json!({"op": "quality", "preset": "quality"}),
                    json!({"success": true, "data": {}}),
                ),
                (json!({"op": "pause"}), json!({"success": true, "data": {}})),
                (
                    json!({"op": "resume"}),
                    json!({"success": true, "data": {}}),
                ),
            ] {
                let started = Instant::now();
                assert_eq!(hot(handle, request.clone()), expected, "{request}");
                assert!(started.elapsed() < Duration::from_millis(10), "{request}");
            }
            for request in [json!({"op": "control", "enabled": true}), input, fit] {
                let started = Instant::now();
                assert_eq!(hot(handle, request.clone())["code"], "IO", "{request}");
                assert!(started.elapsed() < Duration::from_millis(10), "{request}");
            }
            let started = Instant::now();
            assert_eq!(view(handle), (0, PbDesktopViewV1::default()));
            assert!(started.elapsed() < Duration::from_millis(10));
        });
        assert!(
            !disconnecting.is_finished(),
            "the viewer's stop was meant to be still running"
        );
        assert_eq!(disconnecting.join().unwrap()["success"], true);
    });
}

/// A lifecycle request names its op beside its fields, and nothing else.
#[test]
fn lifecycle_requests_refuse_unknown_fields() {
    let connect = json!({"op": "connect", "invitation": "pbd1", "refresh_hz": 60,
        "decoders": {"h264": [1920, 1080], "h265": null}});
    assert!(matches!(
        serde_json::from_value(connect.clone()),
        Ok(Request::Connect(ConnectRequest { max_fps: 0, .. }))
    ));
    let mut unknown = connect;
    unknown["relay"] = json!("relay:7666");
    assert!(serde_json::from_value::<Request>(unknown).is_err());
}

#[test]
fn inspect_invitation_classifies_without_network() {
    // Nothing listens on the default relay, so any network use would fail.
    let handle = test_handle("inspect");
    let inspect = |text: &str| {
        call_json(
            &handle,
            json!({"op": "inspect_invitation", "invitation": text}),
        )
    };
    let invitation = desktop::HostIdentity::generate()
        .unwrap()
        .invitation("desktop:study-pc".into())
        .unwrap();
    assert_eq!(
        inspect(&invitation.encode())["data"],
        json!({"service": "desktop:study-pc", "name": "study-pc",
            "fingerprint": invitation.short_fingerprint(), "format": "pbd1"})
    );
    let base64url = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let v1 = json!({"version": 1, "service": "desktop:study-pc",
        "certificate": base64url.encode(b"certificate"), "token": base64url.encode([7u8; 32])});
    let v1 = inspect(&v1.to_string());
    assert_eq!(
        (&v1["data"]["name"], &v1["data"]["format"]),
        (&json!("study-pc"), &json!("json_v1"))
    );
    let setup = base64::engine::general_purpose::STANDARD
        .encode(json!({"serverAddress": "relay:7666", "msgHeaderKey": "key"}).to_string());
    assert_eq!(inspect(&setup)["code"], "SETUP_STRING");
    assert_eq!(inspect("not an invitation")["code"], "INVALID_INVITATION");
}

#[test]
fn background_consent_and_ui_ownership_boundaries() {
    let handle = test_handle("background-boundaries");
    handle.runtime.block_on(async {
        let config = handle.state.lock().await;
        let lifecycle = handle.desktop.host.lock().await;
        // Neither an old client nor partial consent may reach the service
        // or even wait for application configuration.
        for (privileged, remember) in [(false, true), (true, false)] {
            let request = Request::HostStart(HostStart {
                service: "desktop:background-test".into(),
                display_id: 1,
                allow_control: false,
                relay_only: false,
                privileged,
                remember,
                auto_start: true,
                background: true,
            });
            let failure = tokio::time::timeout(Duration::from_secs(1), call(&handle, request))
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(failure.code(), ErrorCode::InvalidArgument);
        }

        let mut snapshot = unavailable_background("waiting for an interactive session");
        snapshot["configured"] = json!(true);
        snapshot["enabled"] = json!(true);
        handle.desktop.cache_background(snapshot.clone());
        // A blocked service mutation cannot hold up host status, which
        // reads the held lock as busy and still shows the pending
        // background grant.
        let status =
            tokio::time::timeout(Duration::from_secs(1), call(&handle, Request::HostStatus))
                .await
                .unwrap()
                .unwrap();
        assert!(status["host"].is_null());
        assert_eq!(status["busy"], true);
        assert_eq!(status["background"]["enabled"], true);
        drop(lifecycle);

        // UI exit must not load legacy trust, issue background IPC, or
        // clear the service-owned grant, even while settings are locked.
        tokio::time::timeout(
            Duration::from_secs(1),
            call(&handle, Request::HostStop { on_exit: true }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            handle.desktop.background.lock().snapshot.as_ref(),
            Some(&snapshot)
        );
        drop(config);
    });
}

#[tokio::test]
async fn late_background_discovery_cannot_hide_a_new_grant() {
    let state = DesktopState::default();
    let (reply, pending) = tokio::sync::oneshot::channel();
    let mut discovery = std::pin::pin!(discover_background(&state, async {
        pending.await.map_err(err)
    }));
    assert!(
        discovery
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    let mut configured = unavailable_background("");
    configured["configured"] = json!(true);
    configured["enabled"] = json!(true);
    state.cache_background(configured.clone());
    reply.send(unavailable_background("")).unwrap();
    assert_eq!(discovery.await, configured);
    assert_eq!(state.background.lock().snapshot.as_ref(), Some(&configured));
}

#[test]
fn a_host_matches_the_settings_only_on_their_relay_and_namespace() {
    let current = RelayScope {
        server: "relay:7666".into(),
        namespace: Some(1),
    };
    assert!(current.holds("relay:7666", Some(1)));
    assert!(!current.holds("old-relay:7666", Some(1)));
    assert!(!current.holds("relay:7666", Some(2)));
    // The background service's host says so; until the settings are
    // read, it says nothing.
    let mut elsewhere = unavailable_background("");
    elsewhere["configured"] = json!(true);
    elsewhere["server"] = json!("old-relay:7666");
    elsewhere["namespace"] = json!(1);
    assert_eq!(
        with_relay_match(elsewhere.clone(), Some(&current))["matches_relay"],
        false
    );
    assert_eq!(
        with_relay_match(elsewhere, None)["matches_relay"],
        Value::Null
    );
}

/// Resuming publishes the saved host where the settings name now: the
/// bug was a resume that kept publishing on the relay the owner had left.
/// Moving it there changes its invitation, so only a confirmed resume
/// may, whatever the card showed.
#[cfg(windows)]
#[test]
fn resume_configures_the_saved_host_from_the_current_settings() {
    use desktop::privileged::background::{Options, Settings};
    use pb_mapper_client::sdk::RelayTransport;
    let config = crate::state::AppConfig {
        server_address: "new-relay:7666".into(),
        relay_transport: RelayTransport::Quic,
        lan_first: false,
        ..Default::default()
    };
    let nothing = unavailable_background("");
    assert!(resume_options(&nothing, config.clone(), 1, "key".into(), true).is_err());
    let mut saved = unavailable_background("");
    saved["configured"] = json!(true);
    saved["server"] = json!("old-relay:7666");
    saved["namespace"] = json!(1);
    saved["settings"] = json!({"service": "desktop:study", "display_id": 2,
        "allow_control": true, "allow_direct": false, "auto_start": false});
    let unconfirmed = resume_options(&saved, config.clone(), 1, "new-key".into(), false);
    assert_eq!(unconfirmed.unwrap_err().code(), ErrorCode::RelayChanged);
    // The same relay in another namespace is elsewhere too; in the same
    // one it resumes unasked.
    let mut here = saved.clone();
    here["server"] = json!("new-relay:7666");
    let other = resume_options(&here, config.clone(), 2, "new-key".into(), false);
    assert_eq!(other.unwrap_err().code(), ErrorCode::RelayChanged);
    assert!(resume_options(&here, config.clone(), 1, "new-key".into(), false).is_ok());
    assert_eq!(
        resume_options(&saved, config, 1, "new-key".into(), true).unwrap(),
        Options {
            server: "new-relay:7666".into(),
            credential: "new-key".into(),
            relay_transport: RelayTransport::Quic,
            settings: Settings {
                service: "desktop:study".into(),
                display_id: 2,
                display_device: None,
                allow_control: true,
                allow_direct: false,
                auto_start: true,
            },
            lan_first: false,
        }
    );
}

#[cfg(windows)]
#[test]
fn revocation_waits_for_handoff_and_uses_the_actual_owner() {
    let handle = test_handle("revoke-handoff");
    handle.runtime.block_on(async {
        let directory = handle.state.lock().await.desktop_config_dir();
        let scope = trust::Scope {
            directory: directory.clone(),
            relay: "test-relay:7666".into(),
            namespace: 1,
        };
        let pending =
            trust::Saved::for_background(&scope, "desktop:handoff".into(), 1, true, None, None)
                .unwrap();
        trust::save(&directory, &pending).unwrap();
        let lifecycle = handle.desktop.host.lock().await;
        let actual_owner = parking_lot::Mutex::new(false);
        let mut revoke = std::pin::pin!(revoke_trust_with_background(&handle, async {
            assert!(*actual_owner.lock(), "the pending handoff completed first");
            assert!(trust::load(&directory).unwrap().is_none());
            *actual_owner.lock() = false;
            Ok(unavailable_background(""))
        }));
        assert!(
            revoke
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        // Configure completes while the UI still has no background snapshot.
        *actual_owner.lock() = true;
        assert!(handle.desktop.background.lock().snapshot.is_none());
        drop(lifecycle);
        revoke.await.unwrap();
        assert!(!*actual_owner.lock());

        // Failed service revocation stays visible, retaining the observed
        // grant; the removed local pending record cannot resurrect it.
        let configured = json!({"configured": true, "enabled": true});
        handle.desktop.cache_background(configured.clone());
        assert!(
            revoke_trust_with_background(&handle, async {
                Err(err("service management unavailable"))
            })
            .await
            .is_err()
        );
        assert_eq!(
            handle.desktop.background.lock().snapshot.as_ref(),
            Some(&configured)
        );
        assert!(trust::load(&directory).unwrap().is_none());
        std::fs::remove_dir(&directory).unwrap();
    });
}

#[cfg(windows)]
#[tokio::test]
async fn disabled_service_allows_local_host_but_unknown_background_stays_blocked() {
    let state = DesktopState::default();
    let mut configured = unavailable_background("");
    configured["configured"] = json!(true);
    configured["enabled"] = json!(true);
    configured["settings"] = json!({"auto_start": true});
    state.cache_background(configured);

    // A portable UI may share normally when SCM explicitly forbids service
    // restart. No management IPC is required and the saved grant survives.
    let disabled = tokio::time::timeout(
        Duration::from_secs(1),
        query_background_for_helper(
            desktop::privileged::Status {
                installed: true,
                startup_disabled: true,
                ..Default::default()
            },
            std::future::pending(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let disabled = state.cache_background(disabled);
    assert!(background_configured(&disabled));
    assert!(!background_enabled(&disabled));
    assert_eq!(disabled["settings"]["auto_start"], true);
    assert_eq!(disabled["service_disabled"], true);

    // Stopped with automatic recovery pending, Running under another
    // authorized application, and another user's (whose it is is said
    // first, even while it is stopped) all leave a potentially active
    // grant unknown.
    for (running, startup_disabled, owned_by_user, code) in [
        (false, false, true, ErrorCode::HelperStopped),
        (true, true, true, ErrorCode::HelperForeignApp),
        (false, false, false, ErrorCode::HelperOtherOwner),
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            query_background_for_helper(
                desktop::privileged::Status {
                    installed: true,
                    running,
                    startup_disabled,
                    owned_by_user,
                    ..Default::default()
                },
                std::future::pending(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.as_ref().map_err(CtlError::code).err(), Some(code));
        let observed = discover_background(&state, async { result }).await;
        assert!(background_configured(&observed));
        assert_eq!(observed["service_disabled"], false);
    }

    let mut active = disabled;
    active["enabled"] = json!(true);
    active["service_disabled"] = json!(false);
    state.cache_background(active);
    // An installed/running service is not usable if its image cannot be
    // verified. Fail before management IPC and preserve the unknown grant.
    let unverified = query_background_for_helper(
        desktop::privileged::Status {
            installed: true,
            running: true,
            authorized_application: true,
            ..Default::default()
        },
        async { panic!("unverified service must not receive management requests") },
    )
    .await;
    assert_eq!(
        unverified.as_ref().map_err(CtlError::code).err(),
        Some(ErrorCode::HelperUnverified)
    );
    let observed = discover_background(&state, async { unverified }).await;
    assert!(background_configured(&observed));
    assert!(background_enabled(&observed));
    let disconnected = query_background_for_helper(
        desktop::privileged::Status {
            installed: true,
            running: true,
            authorized_application: true,
            identity_verified: true,
            ..Default::default()
        },
        async { Err(err("management connection lost")) },
    )
    .await;
    assert!(disconnected.is_err());
    let observed = discover_background(&state, async { disconnected }).await;
    assert!(background_configured(&observed));
    assert!(background_enabled(&observed));
}

#[cfg(windows)]
#[test]
fn setup_installs_an_outdated_helper_again_and_turns_it_on() {
    use desktop::privileged::Action;
    // Of an older protocol: enabling it again would still refuse to
    // capture, and updating it alone would leave a stopped one off.
    let outdated = desktop::privileged::Status {
        installed: true,
        running: false,
        authorized_application: true,
        identity_verified: true,
        protocol: 2,
        update_required: true,
        ..Default::default()
    };
    assert_eq!(
        helper_setup_action(&outdated),
        Action::Install { enable: true }
    );
    let current = desktop::privileged::Status {
        protocol: desktop::privileged::HELPER_PROTOCOL,
        update_required: false,
        ..outdated
    };
    assert_eq!(helper_setup_action(&current), Action::Enable);
    // Another build, ready: it hosts the old code, so an action the owner
    // started installs this one; startup's restore uses it as it is.
    let other_build = desktop::privileged::Status {
        running: true,
        update_available: true,
        ..current
    };
    assert_eq!(
        helper_setup(&other_build, true),
        Some(Action::Install { enable: true })
    );
    assert_eq!(helper_setup(&other_build, false), None);
    let ready = desktop::privileged::Status {
        update_available: false,
        ..other_build
    };
    assert_eq!(helper_setup(&ready, true), None);
}

#[test]
fn disconnect_cancels_configuration_wait_without_cancelling_reconnect() {
    let handle = test_handle("config-cancel");
    handle.runtime.block_on(async {
        // Contend on the real app-state lock used by the FFI call. Setup
        // must be cancelled while the lock is still held, before loading
        // credentials or contacting a relay.
        let config = handle.state.lock().await;
        let connect_at = |invitation: String, max_fps| {
            call(
                &handle,
                Request::Connect(ConnectRequest {
                    invitation: Some(invitation),
                    computer_id: None,
                    relay_only: false,
                    quality: QualityPreset::Balanced,
                    refresh_hz: 60.0,
                    decoders: DecoderOffer {
                        h264: Some([3840, 2160]),
                        h265: Some([2560, 1440]),
                    },
                    resolution: Some(Resolution::P1440),
                    max_fps,
                }),
            )
        };
        let connect = |invitation| connect_at(invitation, 0);
        let invalid = tokio::time::timeout(Duration::from_secs(1), connect(String::new()))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(invalid.code(), ErrorCode::InvalidInvitation);
        let invitation = invitation().encode();
        // A frame rate no viewer asks for is refused before anything waits.
        let unbounded =
            tokio::time::timeout(Duration::from_secs(1), connect_at(invitation.clone(), 240))
                .await
                .unwrap()
                .unwrap_err();
        assert_eq!(unbounded.code(), ErrorCode::InvalidArgument);
        assert!(handle.desktop.connecting.lock().is_none());
        let mut first = std::pin::pin!(connect(invitation.clone()));
        let mut context = Context::from_waker(Waker::noop());
        assert!(first.as_mut().poll(&mut context).is_pending());
        tokio::time::timeout(Duration::from_secs(1), call(&handle, Request::Disconnect))
            .await
            .unwrap()
            .unwrap();

        // A new attempt can start before the cancelled call resumes.
        // Finishing the old call must not unregister the new token.
        let mut second = std::pin::pin!(connect(invitation));
        assert!(second.as_mut().poll(&mut context).is_pending());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), first)
                .await
                .unwrap()
                .is_err()
        );
        assert!(handle.desktop.connecting.lock().is_some());
        call(&handle, Request::Disconnect).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), second)
                .await
                .unwrap()
                .is_err()
        );
        drop(config);
        assert!(handle.desktop.viewer.read().is_none());
        assert!(handle.desktop.connecting.lock().is_none());
    });
}

/// Without a credential no retry can succeed: the attempt ends with a
/// reason the person can act on, not an IO failure the viewer retries.
#[test]
fn connect_without_a_credential_ends_the_attempt() {
    let handle = test_handle("no-credential");
    let _credential_guard =
        pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK.blocking_lock();
    pb_mapper_core::checksum::set_process_msg_header_key(None).unwrap();
    let failure = handle
        .runtime
        .block_on(call(
            &handle,
            Request::Connect(ConnectRequest {
                invitation: Some(invitation().encode()),
                computer_id: None,
                relay_only: false,
                quality: QualityPreset::Balanced,
                refresh_hz: 60.0,
                decoders: DecoderOffer {
                    h264: None,
                    h265: None,
                },
                resolution: None,
                max_fps: 0,
            }),
        ))
        .unwrap_err();
    assert_eq!(failure.code(), ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn disconnect_cancels_setup_while_the_last_viewer_stops() {
    let state = Arc::new(DesktopState::default());
    let (viewer, _host) = scripted_viewer().await;
    state
        .connect_viewer(|_| async { Ok((viewer, connected())) })
        .await
        .unwrap();
    // The last viewer's stop is held: its host never closes.
    let stopping = tokio::spawn({
        let state = state.clone();
        async move { state.disconnect_viewer().await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.viewer.read().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // A new attempt registers at once, and the next disconnect cancels it
    // without waiting for that stop.
    let (entered, started) = tokio::sync::oneshot::channel();
    let attempt = tokio::spawn({
        let state = state.clone();
        async move {
            state
                .connect_viewer(|_| async {
                    let _ = entered.send(());
                    std::future::pending::<Result<(Viewer, Connected), CtlError>>().await
                })
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), started)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), state.disconnect_viewer())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), attempt)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(!stopping.is_finished());
    stopping.await.unwrap().unwrap();
    assert!(state.viewer.read().is_none());
    assert!(state.connecting.lock().is_none());
}

#[tokio::test]
async fn disconnect_cancels_pending_setup_without_waiting_for_network() {
    let state = Arc::new(DesktopState::default());
    let (entered, started) = tokio::sync::oneshot::channel();
    let connecting = state.clone();
    let attempt = tokio::spawn(async move {
        connecting
            .connect_viewer(|stage| async move {
                stage.store(stage_code(ConnectStage::Identity), Ordering::Release);
                let _ = entered.send(());
                std::future::pending::<Result<(Viewer, Connected), CtlError>>().await
            })
            .await
    });
    started.await.unwrap();
    // The attempt reports how far it got.
    assert_eq!(
        state.viewer_status(None).unwrap(),
        json!({"state": "connecting", "stage": "identity"})
    );
    tokio::time::timeout(Duration::from_secs(1), state.disconnect_viewer())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), attempt)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(state.viewer.read().is_none());
    assert!(state.connecting.lock().is_none());
    assert_eq!(state.viewer_status(None).unwrap(), Value::Null);
}
