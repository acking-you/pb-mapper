//! `RemoteBackend` over in-memory pipes: to scripted workers, which write
//! the records a test needs in the order it needs them, and to the helper's
//! own worker loop (`server::work`) on a synthetic display.
use super::*;
use crate::{
    native::PlatformBackend,
    oop::server::{self, NamedAdapter, ServeState},
    platform::{
        Platform,
        synthetic::{Probe, Synthetic},
    },
    scheduler::{Waiter, Wake},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::DuplexStream;

/// What a scripted worker writes for one request: records, each with its
/// byte record when it has one.
type Script = Vec<(Record, Option<Vec<u8>>)>;

/// The answer `response`, with an empty byte record.
fn reply(response: Response) -> Script {
    vec![(Record::Reply(Box::new(response)), Some(vec![]))]
}

/// A unit of the desktop of `generation`, with the units its encoder still
/// holds, its bytes `marker`.
fn unit(generation: u64, pending: u32, marker: u8) -> (Record, Option<Vec<u8>>) {
    let unit = Record::Unit {
        generation,
        video: crate::VideoFormat {
            codec: VideoCodec::H264,
            width: 64,
            height: 64,
            ..crate::VideoFormat::default()
        },
        pending,
        collected_unix_us: None,
    };
    (unit, Some(vec![marker]))
}

/// A pointer push.
fn pointer(x: i32, y: i32) -> (Record, Option<Vec<u8>>) {
    let push = Record::Pointer {
        cursor: Some(CursorPosition {
            x,
            y,
            visible: true,
        }),
        shape_serial: None,
    };
    (push, None)
}

/// The host's end of an in-memory pipe whose worker writes what `script`
/// makes of each request, in order, until `Close`, or until the host stops
/// reading. A byte record is written as it is, however long.
fn scripted(
    script: impl Fn(&Request) -> Script + Send + 'static,
) -> (DuplexStream, std::thread::JoinHandle<()>) {
    let (pipe, mut worker) = tokio::io::duplex(1 << 16);
    let helper = std::thread::spawn(move || {
        runtime().block_on(async move {
            while let Ok(request) = ipc::read_json::<_, Request>(&mut worker).await {
                for (record, bytes) in script(&request) {
                    let mut written = ipc::write_json(&mut worker, &record).await;
                    if let (Ok(()), Some(bytes)) = (&written, bytes) {
                        written = ipc::write_bytes(&mut worker, &bytes, usize::MAX).await;
                    }
                    if written.is_err() {
                        return;
                    }
                }
                if matches!(request, Request::Close {}) {
                    return;
                }
            }
        });
    });
    (pipe, helper)
}

/// A `RemoteBackend` over an in-memory pipe whose worker answers each
/// request with `answer` and an empty image, until `Close`.
fn remote(
    answer: impl Fn(&Request) -> Response + Send + 'static,
) -> (RemoteBackend, std::thread::JoinHandle<()>) {
    pushing(move |request| reply(answer(request)))
}

/// A `RemoteBackend` over an in-memory pipe whose worker writes what
/// `script` makes of each request.
fn pushing(
    script: impl Fn(&Request) -> Script + Send + 'static,
) -> (RemoteBackend, std::thread::JoinHandle<()>) {
    let (pipe, helper) = scripted(script);
    let backend = RemoteBackend::new(runtime(), pipe, BackendCaps::default()).unwrap();
    (backend, helper)
}

/// [`remote`], started: its first request is `Start`.
fn started(
    answer: impl Fn(&Request) -> Response + Send + 'static,
) -> (RemoteBackend, std::thread::JoinHandle<()>) {
    let (pipe, helper) = scripted(move |request| reply(answer(request)));
    let backend = RemoteBackend::start(runtime(), pipe, 1, None, BackendCaps::default()).unwrap();
    (backend, helper)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn desktop() -> DesktopContext {
    DesktopContext {
        display: crate::Display {
            id: 1,
            name: "helper".into(),
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        },
        generation: 1,
        desktop: crate::DesktopKind::Ordinary,
    }
}

fn rate() -> crate::VideoRate {
    crate::VideoRate {
        bitrate: 4_000_000,
        pace_fps: 60,
    }
}

/// Thread C's wake, bound to `backend`, which turns the worker's pushes on;
/// the waiter it wakes.
fn bound(backend: &mut RemoteBackend) -> Waiter {
    let wake = Wake::new();
    let waiter = Waiter::new(&wake).unwrap();
    backend.bind(Waker(wake));
    waiter
}

/// Wait on `waiter` until `done`, each wait ended by a wake within a
/// second: what this waits for is a push that wakes thread C, which would
/// otherwise see it only at its next deadline.
fn woken(waiter: &Waiter, mut done: impl FnMut() -> bool) {
    while !done() {
        assert!(
            waiter
                .wait(Instant::now() + Duration::from_secs(1))
                .unwrap(),
            "no wake"
        );
    }
}

/// A `RemoteBackend` started over an in-memory pipe to a worker that runs
/// the helper's loop (`server::work`) on a synthetic display on `probe`,
/// whose encoder pushes (`PUSH`) or is polled, on the GPU `adapter` names,
/// as the helper's worker runs it; `seen` is shown each request first.
pub(crate) fn synthetic<const PUSH: bool>(
    probe: &Probe,
    journal: std::path::PathBuf,
    adapter: fn(&crate::Display) -> Option<NamedAdapter>,
    mut seen: impl FnMut(&Request) + Send + 'static,
) -> (RemoteBackend, std::thread::JoinHandle<()>) {
    let (pipe, mut worker) = tokio::io::duplex(1 << 16);
    let worker_probe = probe.clone();
    let helper = std::thread::spawn(move || {
        runtime().block_on(async move {
            // The service reads the host's `Start`; the worker answers.
            let Request::Start { display_device, .. } = ipc::read_json(&mut worker).await.unwrap()
            else {
                panic!("expected Start");
            };
            let (requests, calls) = std::sync::mpsc::sync_channel(4);
            let (out, mut outgoing) = tokio::sync::mpsc::channel(16);
            let wake = Wake::new();
            let stop = Arc::new(AtomicBool::new(false));
            let (woken, stopped) = (wake.clone(), stop.clone());
            let capture = std::thread::spawn(move || {
                let parts = Synthetic::<PUSH>::open(&worker_probe, &Probe::display()).unwrap();
                let mut backend = PlatformBackend::from_parts(
                    Arc::new(worker_probe),
                    parts,
                    crate::modes::Journal::in_dir(Some(journal)),
                );
                let waiter = Waiter::new(&woken).unwrap();
                backend.bind(Waker(woken));
                let mut state = ServeState::new(adapter);
                server::work(&mut backend, &mut state, &calls, &out, &waiter, &|| {
                    stopped.load(Ordering::Acquire)
                });
            });
            server::exchange(
                &mut worker,
                1,
                display_device,
                |request| {
                    seen(&request);
                    requests.try_send(request).map_err(error)?;
                    wake.notify();
                    Ok(())
                },
                &mut outgoing,
            )
            .await
            .unwrap();
            stop.store(true, Ordering::Release);
            wake.notify();
            drop(outgoing);
            capture.join().unwrap();
        });
    });
    let backend = RemoteBackend::start(
        runtime(),
        pipe,
        1,
        Some(Probe::display().name),
        BackendCaps::default(),
    )
    .unwrap();
    (backend, helper)
}

/// The desktop kind crosses the pipe as it always did.
#[test]
fn the_desktop_kind_crosses_the_pipe_as_its_string() {
    let secure = DesktopContext {
        desktop: crate::DesktopKind::Secure,
        ..desktop()
    };
    assert_eq!(
        serde_json::to_string(&secure).unwrap(),
        r#"{"display":{"id":1,"name":"helper","x":0,"y":0,"width":64,"height":64},"generation":1,"desktop":"secure"}"#
    );
    let ordinary: DesktopContext = serde_json::from_str(
        r#"{"display":{"id":1,"name":"helper","x":0,"y":0,"width":64,"height":64},"generation":1,"desktop":"ordinary"}"#,
    )
    .unwrap();
    assert_eq!(ordinary, desktop());
}

/// Protocol 8 over a pipe to a worker serving a synthetic display: its
/// `Start` answer says what it can do, H.265 is asked only for a viewer
/// that decodes it, the pointer's shape crosses once per new shape, its
/// modes cross, a change and its restore apply there, and a failure reads
/// back as the kind the worker gave it.
#[test]
fn protocol_8_carries_caps_shapes_modes_and_failure_kinds() {
    let (probe, modes) = Probe::with_modes();
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop");
    let asked_h265 = Arc::new(AtomicUsize::new(0));
    let asked_shape = Arc::new(AtomicUsize::new(0));
    let (asked, asked_for_shape) = (asked_h265.clone(), asked_shape.clone());
    let (mut backend, helper) = synthetic::<false>(
        &probe,
        journal.to_path_buf(),
        |_| None,
        move |request| {
            if matches!(request, Request::H265 {}) {
                asked.fetch_add(1, Ordering::SeqCst);
            }
            if matches!(request, Request::CursorShape {}) {
                asked_for_shape.fetch_add(1, Ordering::SeqCst);
            }
        },
    );
    let described = backend.describe(false).unwrap();
    assert!(described.caps.display_modes && !described.caps.h265);
    assert_eq!(described.caps.os, Some(crate::HostOs::Linux));
    assert_eq!(asked_h265.load(Ordering::SeqCst), 0);
    backend.describe(true).unwrap();
    assert_eq!(asked_h265.load(Ordering::SeqCst), 1);
    assert!(described.caps.cursor_shapes);

    // The first ask turns the worker's shape reads on; after it, only a
    // newer shape crosses again. A pointer on another display is not read:
    // its shape crosses once it is back on the shared one.
    let display = Probe::display();
    probe.point(display.x - 1, 1);
    let shape = |kind| crate::CursorShape::new(kind, 2, 2, (1, 1), vec![3; 16]).unwrap();
    probe.set_shape(shape(crate::CursorKind::Ibeam));
    let context = backend.context().unwrap();
    assert_eq!(backend.cursor_shape(&context).unwrap(), None);
    probe.point(display.x + 1, 1);
    backend.context().unwrap();
    assert_eq!(
        backend.cursor_shape(&context).unwrap(),
        Some(shape(crate::CursorKind::Ibeam))
    );
    backend.context().unwrap();
    assert_eq!(backend.cursor_shape(&context).unwrap(), None);
    probe.set_shape(shape(crate::CursorKind::Custom));
    backend.context().unwrap();
    assert_eq!(
        backend.cursor_shape(&context).unwrap(),
        Some(shape(crate::CursorKind::Custom))
    );
    assert_eq!(asked_shape.load(Ordering::SeqCst), 3);

    let context = backend.context().unwrap();
    let listed = backend.display_modes(&context).unwrap().unwrap();
    assert_eq!(listed.supported, modes);
    backend.set_display_mode(&context, modes[1]).unwrap();
    assert_eq!(backend.display().unwrap().width, modes[1].width);
    assert!(backend.restore_display_mode().unwrap());
    assert_eq!(backend.display().unwrap().width, Probe::display().width);

    probe.lose(true);
    let lost = backend.context().unwrap_err();
    assert_eq!(
        crate::failure_kind(&lost),
        Some(crate::FailureKind::DisplayLost)
    );
    drop(backend);
    helper.join().unwrap();
}

/// Protocol 10 over a pipe to a worker serving a synthetic display: a
/// capture that found nothing new says so, and leaves thread C's tick
/// armed, while what the encoder finished still reaches thread C.
#[test]
fn protocol_10_says_when_a_capture_found_nothing_new() {
    let probe = Probe::default();
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-still");
    let (mut backend, helper) = synthetic::<false>(&probe, journal.to_path_buf(), |_| None, |_| {});
    let context = backend.context().unwrap();
    backend
        .configure_video(crate::VideoLimits::for_viewer(60.0), 60)
        .unwrap();
    let rate = crate::VideoRate {
        bitrate: crate::qos::ENCODER_BITRATE_MIN,
        pace_fps: 60,
    };
    let capture = |backend: &mut RemoteBackend| {
        let unit = backend
            .capture_video(&context, crate::VideoCodec::H264, false, rate)
            .unwrap();
        (usize::from(unit.is_some()), backend.submitted_video())
    };
    let (first, submitted) = capture(&mut backend);
    assert!(submitted);
    probe.still(true);
    let (second, submitted) = capture(&mut backend);
    assert!(!submitted);
    // The first capture's unit, which the worker pushed before the second
    // capture's answer: handed over by a capture, or waiting to be drained.
    let mut units = first + second;
    while backend.pending_video() > 0 {
        let drained = backend.drain_video(&context, Duration::ZERO).unwrap();
        units += usize::from(drained.is_some());
    }
    assert_eq!(units, 1);
    // A drain says nothing of a tick.
    assert!(!backend.submitted_video());
    probe.still(false);
    let (_, submitted) = capture(&mut backend);
    assert!(submitted);
    drop(backend);
    helper.join().unwrap();
}

/// `submitted` crosses only when false: a capture whose answer has none
/// spent the tick.
#[test]
fn an_absent_submitted_means_the_tick_was_spent() {
    assert!(
        !serde_json::to_string(&Response::default())
            .unwrap()
            .contains("submitted")
    );
    let still = Arc::new(AtomicBool::new(true));
    let worker_still = still.clone();
    let reported = desktop();
    let (mut backend, helper) = remote(move |request| {
        let capture = matches!(request, Request::Video { .. });
        let still = worker_still.load(Ordering::SeqCst);
        Response {
            context: Some(reported.clone()),
            submitted: (capture && still).then_some(false),
            ..Response::default()
        }
    });
    let codec = crate::VideoCodec::H264;
    backend
        .capture_video(&desktop(), codec, false, rate())
        .unwrap();
    assert!(!backend.submitted_video());
    still.store(false, Ordering::SeqCst);
    backend
        .capture_video(&desktop(), codec, false, rate())
        .unwrap();
    assert!(backend.submitted_video());
    drop(backend);
    helper.join().unwrap();
}

/// An armed tick waits for the worker, at most the liveness interval: a
/// keyframe thread C wants sooner is asked for once, and a capture that
/// asked for one and found nothing new waits for the worker's encoder,
/// which keeps it, rather than asking again at once.
#[test]
fn an_armed_tick_asks_for_a_keyframe_once() {
    let (mut backend, helper) = remote(|request| Response {
        context: Some(desktop()),
        submitted: matches!(request, Request::Video { .. }).then_some(false),
        ..Response::default()
    });
    let codec = crate::VideoCodec::H264;
    let before = Instant::now();
    backend
        .capture_video(&desktop(), codec, false, rate())
        .unwrap();
    let after = Instant::now();
    let until = backend.capture_idle_until(None).unwrap();
    assert!(until >= before + LIVENESS_INTERVAL && until <= after + LIVENESS_INTERVAL);
    let due = after + Duration::from_millis(5);
    assert_eq!(backend.capture_idle_until(Some(due)), Some(due));
    backend
        .capture_video(&desktop(), codec, true, rate())
        .unwrap();
    let until = backend.capture_idle_until(None).unwrap();
    assert_eq!(backend.capture_idle_until(Some(due)), Some(until));
    drop(backend);
    helper.join().unwrap();
}

/// Protocol 11 over a pipe to workers serving a synthetic display, one per
/// session as the helper starts them. A worker names its GPU only for a
/// viewer that decodes H.265. The first on a GPU is asked about H.265; a
/// later one on the same GPU and driver is not, and says what the first's
/// probe said. A new driver is asked again, and so is every worker that
/// names no GPU, or whose answer was no probe's of it.
#[test]
fn a_later_worker_on_the_same_gpu_and_driver_is_not_asked_about_h265() {
    let probe = Probe::default();
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-h265-kept");
    let named = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(AtomicUsize::new(0));
    let session = |adapter: fn(&crate::Display) -> Option<NamedAdapter>| {
        let (named, asked) = (named.clone(), asked.clone());
        let (mut backend, helper) =
            synthetic::<false>(&probe, journal.to_path_buf(), adapter, move |request| {
                match request {
                    Request::Adapter {} => named.fetch_add(1, Ordering::SeqCst),
                    Request::H265 {} => asked.fetch_add(1, Ordering::SeqCst),
                    _ => 0,
                };
            });
        // The synthetic display encodes H.264 alone.
        assert!(!backend.describe(false).unwrap().caps.h265);
        assert!(!backend.describe(true).unwrap().caps.h265);
        drop(backend);
        helper.join().unwrap();
    };
    // An AMD GPU, on one driver and then on the next, as the worker's probe
    // of it answered; then on a third, found after a probe that had found no
    // GPU for the display.
    fn gpu(driver: u64) -> Option<AdapterKey> {
        AdapterKey::new(0x1002, 0x744c, 0x5315_1849, 0xc8, driver)
    }
    let first_driver: fn(&crate::Display) -> Option<NamedAdapter> = |_| {
        Some(NamedAdapter {
            key: gpu(1)?,
            h265: Some(false),
        })
    };
    let next_driver: fn(&crate::Display) -> Option<NamedAdapter> = |_| {
        Some(NamedAdapter {
            key: gpu(2)?,
            h265: Some(false),
        })
    };
    let unprobed: fn(&crate::Display) -> Option<NamedAdapter> = |_| {
        Some(NamedAdapter {
            key: gpu(3)?,
            h265: None,
        })
    };
    session(first_driver);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    session(first_driver);
    assert_eq!(asked.load(Ordering::SeqCst), 1, "the same GPU and driver");
    session(next_driver);
    assert_eq!(asked.load(Ordering::SeqCst), 2, "a new driver");
    session(next_driver);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    session(unprobed);
    session(unprobed);
    assert_eq!(asked.load(Ordering::SeqCst), 4, "a no without a probe");
    session(|_| None);
    session(|_| None);
    assert_eq!(asked.load(Ordering::SeqCst), 6, "no GPU named");
    assert_eq!(named.load(Ordering::SeqCst), 8, "once per H.265 viewer");
}

/// An answer of yes is kept as a no is, until that GPU's H.265 encoder
/// fails, as thread C then turns H.265 off: the next worker on it is asked
/// again.
#[test]
fn an_h265_encoder_failure_drops_the_answer_kept_for_its_gpu() {
    // An Intel GPU on driver 31.0.101.5333.
    let adapter = AdapterKey::new(
        0x8086,
        0x56a0,
        0x1020_8086,
        8,
        (31 << 48) | (101 << 16) | 5333,
    );
    let encoder_failed = FailureKind::EncoderFailed {
        codec: VideoCodec::H265,
    };
    let asked = Arc::new(AtomicUsize::new(0));
    let session = || {
        let asked = asked.clone();
        started(move |request| {
            let mut response = Response {
                context: Some(desktop()),
                ..Response::default()
            };
            match request {
                Request::Adapter {} => response.adapter = adapter,
                Request::H265 {} => {
                    asked.fetch_add(1, Ordering::SeqCst);
                    response.h265 = Some(true);
                    response.adapter = adapter;
                }
                Request::Video { .. } => {
                    response.error = Some("the HEVC encoder took no input".into());
                    response.failure = Some(encoder_failed);
                }
                _ => {}
            }
            response
        })
    };
    for kept in [false, true] {
        let (mut backend, helper) = session();
        assert!(backend.describe(true).unwrap().caps.h265);
        assert_eq!(asked.load(Ordering::SeqCst), 1, "kept: {kept}");
        drop(backend);
        helper.join().unwrap();
    }
    let (mut backend, helper) = session();
    assert!(backend.describe(true).unwrap().caps.h265);
    let Err(failed) = backend.capture_video(&desktop(), VideoCodec::H265, true, rate()) else {
        panic!("the worker's encoder failed");
    };
    assert_eq!(crate::failure_kind(&failed), Some(encoder_failed));
    drop(backend);
    helper.join().unwrap();
    let (mut backend, helper) = session();
    assert!(backend.describe(true).unwrap().caps.h265);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    drop(backend);
    helper.join().unwrap();
}

/// The process keeps the answers of at most eight GPUs, dropping the
/// oldest; a GPU answered again replaces its answer.
#[test]
fn the_kept_h265_answers_are_bounded() {
    let adapter = |device| AdapterKey::new(0x10de, device, 0, 0, 1).unwrap();
    let mut answers = H265Answers::new();
    for device in 0..=8 {
        answers.put(adapter(device), device % 2 == 0);
    }
    assert_eq!(answers.0.len(), H265_ANSWERS_KEPT);
    assert_eq!(answers.get(&adapter(0)), None);
    for device in 1..=8 {
        assert_eq!(answers.get(&adapter(device)), Some(device % 2 == 0));
    }
    answers.put(adapter(1), true);
    assert_eq!(answers.0.len(), H265_ANSWERS_KEPT);
    assert_eq!(answers.get(&adapter(1)), Some(true));
    assert_eq!(answers.get(&adapter(2)), Some(true));
    answers.forget(&adapter(1));
    assert_eq!(answers.get(&adapter(1)), None);
}

/// A lost pipe fails the call that met it and every later one with
/// `NotConnected`, which ends the session; nothing is sent again.
#[test]
fn a_lost_helper_fails_every_later_call_as_not_connected() {
    let (pipe, worker) = tokio::io::duplex(1 << 16);
    drop(worker);
    let mut backend = RemoteBackend::new(runtime(), pipe, BackendCaps::default()).unwrap();
    let lost = backend.context().unwrap_err();
    assert_eq!(lost.kind(), std::io::ErrorKind::NotConnected);
    assert_eq!(crate::failure_kind(&lost), None);
    for later in [
        backend.capture().unwrap_err(),
        backend.cursor(&desktop()).unwrap_err(),
        backend
            .drain_video(&desktop(), Duration::ZERO)
            .map(|_| ())
            .unwrap_err(),
    ] {
        assert_eq!(later.kind(), std::io::ErrorKind::NotConnected);
    }
}

#[test]
fn a_helper_error_keeps_the_desktop_and_its_pointer() {
    let desktop = desktop();
    let pointer = CursorPosition {
        x: 3,
        y: 4,
        visible: true,
    };
    // A helper whose LockWorkStation fails: Input gets an error, Release and
    // Close report no desktop, Presented reports it without looking at the
    // pointer, and the rest report both.
    let reported = desktop.clone();
    let (mut backend, helper) = remote(move |request| {
        let mut response = Response::default();
        match request {
            Request::Input { .. } => response.error = Some("lock failed".into()),
            Request::Release {} | Request::Close {} => {}
            Request::Presented { .. } => response.context = Some(reported.clone()),
            _ => {
                response.context = Some(reported.clone());
                response.cursor = Some(pointer);
            }
        }
        response
    });
    assert_eq!(backend.context().unwrap(), desktop);
    assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
    // Neither a failed lock nor a release says the desktop changed, and a
    // lease renewal does not observe the pointer.
    assert!(backend.input_in_context(&desktop, &Input::Lock {}).is_err());
    assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
    backend.release();
    assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
    backend.presented(&desktop).unwrap();
    assert_eq!(backend.cursor(&desktop).unwrap(), Some(pointer));
    drop(backend);
    helper.join().unwrap();
}

/// A shape whose bitmap is not the size its header gives is refused, and
/// not asked for again until the worker numbers a newer one.
#[test]
fn a_misfit_helper_shape_is_refused() {
    let desktop = desktop();
    let reported = desktop.clone();
    let (mut backend, helper) = remote(move |request| Response {
        context: Some(reported.clone()),
        shape_serial: request.observes_cursor().then_some(1),
        // A pixel's header over the empty record `remote` writes.
        shape: matches!(request, Request::CursorShape {}).then_some(ShapeHeader {
            kind: crate::CursorKind::Arrow,
            width: 1,
            height: 1,
            hot_x: 0,
            hot_y: 0,
        }),
        ..Response::default()
    });
    backend.context().unwrap();
    let refused = backend.cursor_shape(&desktop).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(backend.cursor_shape(&desktop).unwrap(), None);
    drop(backend);
    helper.join().unwrap();
}

#[test]
fn a_helper_hosted_unit_carries_the_encoder_gop() {
    // The worker's encoder read back an endless GOP; thread C reports the GOP
    // of the units it takes from this backend.
    let (mut backend, helper) = remote(|request| Response {
        video: matches!(request, Request::Video { .. }).then(|| crate::VideoFormat {
            codec: crate::VideoCodec::H264,
            width: 64,
            height: 64,
            keyframe: true,
            gop: u32::MAX,
            ..crate::VideoFormat::default()
        }),
        ..Response::default()
    });
    let unit = backend
        .capture_video(&desktop(), crate::VideoCodec::H264, true, rate())
        .unwrap()
        .unwrap();
    assert_eq!(unit.format.gop, u32::MAX);
    drop(backend);
    helper.join().unwrap();
}

/// Protocol 12 over a pipe to a worker serving a pushing encoder: the unit
/// a capture submitted reaches thread C with no drain request, waking it,
/// and the units pending count what the worker's encoder holds and what
/// thread C has not taken, together.
#[test]
fn units_arrive_without_a_drain_request() {
    let probe = Probe::default();
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-push");
    let asked = Arc::new(AtomicUsize::new(0));
    let requests = asked.clone();
    let (mut backend, helper) = synthetic::<true>(
        &probe,
        journal.to_path_buf(),
        |_| None,
        move |_| {
            requests.fetch_add(1, Ordering::SeqCst);
        },
    );
    let waiter = bound(&mut backend);
    let context = backend.context().unwrap();
    backend
        .configure_video(crate::VideoLimits::for_viewer(60.0), 60)
        .unwrap();
    let rate = crate::VideoRate {
        bitrate: crate::qos::ENCODER_BITRATE_MIN,
        pace_fps: 60,
    };
    let before = asked.load(Ordering::SeqCst);
    // The encoder finishes the unit a millisecond after the submit, on a
    // thread of its own.
    let early = backend
        .capture_video(&context, VideoCodec::H264, true, rate)
        .unwrap();
    let unit = match early {
        Some(unit) => unit,
        None => {
            assert_eq!(backend.pending_video(), 1, "the worker's encoder holds it");
            woken(&waiter, || !backend.link.shared.lock().queue.is_empty());
            assert_eq!(backend.pending_video(), 1, "pushed, and not taken yet");
            backend
                .drain_video(&context, Duration::ZERO)
                .unwrap()
                .unwrap()
        }
    };
    assert!(unit.format.keyframe);
    assert_eq!(backend.pending_video(), 0);
    assert_eq!(
        asked.load(Ordering::SeqCst) - before,
        1,
        "the capture alone"
    );
    drop(backend);
    helper.join().unwrap();
}

/// A unit of a desktop thread C no longer expects is dropped as it is
/// taken; the units pending count each record as it comes.
#[test]
fn a_unit_of_another_desktop_is_dropped_and_pending_counts_each_push() {
    let (mut backend, helper) = pushing(|request| {
        let (mut script, pending) = match request {
            Request::Video { .. } => (vec![unit(7, 2, 7), unit(1, 1, 1)], 2),
            Request::Presented { .. } => (vec![unit(1, 1, 2)], 1),
            _ => (vec![], 0),
        };
        script.extend(reply(Response {
            context: Some(desktop()),
            pending,
            ..Response::default()
        }));
        script
    });
    let desktop = desktop();
    // The capture's answer says the encoder holds two, after the units
    // pushed before it: the stale one goes, and the current one is taken.
    let taken = backend
        .capture_video(&desktop, VideoCodec::H264, false, rate())
        .unwrap();
    assert_eq!(taken.unwrap().bytes, [1]);
    assert_eq!(backend.pending_video(), 2);
    // One more pushed, and one left in the encoder.
    backend.presented(&desktop).unwrap();
    assert_eq!(backend.pending_video(), 2);
    let next = backend.drain_video(&desktop, Duration::ZERO).unwrap();
    assert_eq!(next.unwrap().bytes, [2]);
    assert_eq!(backend.pending_video(), 1);
    // Taken for a desktop of another generation, the next is dropped.
    backend.presented(&desktop).unwrap();
    assert_eq!(backend.pending_video(), 2);
    let other = DesktopContext {
        generation: 2,
        ..desktop.clone()
    };
    assert!(
        backend
            .drain_video(&other, Duration::ZERO)
            .unwrap()
            .is_none()
    );
    assert_eq!(backend.pending_video(), 1);
    drop(backend);
    helper.join().unwrap();
}

/// A failure the worker's encoder met is taken in the units' order, of the
/// kind the worker gave it.
#[test]
fn a_pushed_failure_is_taken_in_order() {
    let failure = FailureKind::EncoderFailed {
        codec: VideoCodec::H264,
    };
    let (mut backend, helper) = pushing(move |request| {
        let mut script = vec![];
        if matches!(request, Request::Presented { .. }) {
            script.push(unit(1, 1, 1));
            let failed = Record::Failed {
                error: "hardware video encoder stalled".into(),
                failure: Some(failure),
                pending: 0,
            };
            script.push((failed, None));
        }
        script.extend(reply(Response {
            context: Some(desktop()),
            ..Response::default()
        }));
        script
    });
    let desktop = desktop();
    backend.presented(&desktop).unwrap();
    assert_eq!(backend.pending_video(), 2);
    let first = backend.drain_video(&desktop, Duration::ZERO).unwrap();
    assert_eq!(first.unwrap().bytes, [1]);
    let failed = backend
        .drain_video(&desktop, Duration::ZERO)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(crate::failure_kind(&failed), Some(failure));
    assert_eq!(backend.pending_video(), 0);
    drop(backend);
    helper.join().unwrap();
}

/// A unit longer than any record may be, more units than an encoder holds,
/// or more pending than a worker may report, break the protocol: the link
/// is dropped, and every later call fails as `NotConnected`.
#[test]
fn an_oversize_unit_or_one_too_many_is_refused() {
    for broken in 0..3 {
        let (mut backend, helper) = pushing(move |request| {
            let mut script = vec![];
            if matches!(request, Request::Presented { .. }) {
                match broken {
                    0 => {
                        let (record, _) = unit(1, 0, 0);
                        script.push((record, Some(vec![0; ipc::MAX_FRAME + 1])));
                    }
                    1 => script.extend((0..=QUEUED_MAX).map(|_| unit(1, 0, 0))),
                    _ => script.push(unit(1, PENDING_MAX + 1, 0)),
                }
            }
            script.extend(reply(Response {
                context: Some(desktop()),
                ..Response::default()
            }));
            script
        });
        let desktop = desktop();
        backend.context().unwrap();
        let refused = backend.presented(&desktop).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::NotConnected, "{broken}");
        let later = backend.context().unwrap_err();
        assert_eq!(later.kind(), std::io::ErrorKind::NotConnected);
        drop(backend);
        helper.join().unwrap();
    }
}

/// A pushed pointer outside the display the worker last reported is not
/// taken: the last one stays. One inside replaces it.
#[test]
fn an_out_of_display_pointer_is_refused() {
    let step = Arc::new(AtomicUsize::new(0));
    let (mut backend, helper) = pushing(move |request| {
        let mut script = vec![];
        if matches!(request, Request::Presented { .. }) {
            script.push(match step.fetch_add(1, Ordering::SeqCst) {
                0 => pointer(64, 10),
                1 => pointer(-1, 10),
                _ => pointer(5, 6),
            });
        }
        script.extend(reply(Response {
            context: Some(desktop()),
            cursor: Some(CursorPosition {
                x: 3,
                y: 4,
                visible: true,
            }),
            ..Response::default()
        }));
        script
    });
    let desktop = desktop();
    backend.context().unwrap();
    let at = |backend: &mut RemoteBackend| {
        let cursor = backend.cursor(&desktop).unwrap().unwrap();
        (cursor.x, cursor.y)
    };
    assert_eq!(at(&mut backend), (3, 4));
    for _ in 0..2 {
        backend.presented(&desktop).unwrap();
        assert_eq!(at(&mut backend), (3, 4));
    }
    backend.presented(&desktop).unwrap();
    assert_eq!(at(&mut backend), (5, 6));
    drop(backend);
    helper.join().unwrap();
}

/// A capture's answer dates its picture by its age: an age over a second
/// is no date, and the picture paces on the grid.
#[test]
fn an_age_over_a_second_is_ignored() {
    let (mut backend, helper) = pushing(|request| {
        let (presented_ago_us, presented_count) = match request {
            Request::Video { keyframe: true, .. } => (Some(2_000_000), Some(1)),
            Request::Video { .. } => (Some(1_500), Some(3)),
            _ => (None, None),
        };
        reply(Response {
            context: Some(desktop()),
            presented_ago_us,
            presented_count,
            ..Response::default()
        })
    });
    let desktop = desktop();
    backend
        .capture_video(&desktop, VideoCodec::H264, true, rate())
        .unwrap();
    assert!(backend.submitted_video());
    assert_eq!(backend.submitted_picture(), None);
    let before = Instant::now();
    backend
        .capture_video(&desktop, VideoCodec::H264, false, rate())
        .unwrap();
    let after = Instant::now();
    let picture = backend.submitted_picture().unwrap();
    assert_eq!(picture.presents, 3);
    let age = Duration::from_micros(1_500);
    assert!(picture.at >= before - age && picture.at <= after - age);
    drop(backend);
    helper.join().unwrap();
}

/// Answers and pushes are kept in the order the worker wrote them: a unit
/// pushed before a capture's answer goes before the one the answer carries;
/// a pointer pushed before an answer that observed the pointer gives way to
/// it, and one pushed after it replaces it; a fresh picture told before a
/// capture's answer is the one that capture took, and only one told after
/// it wants the armed tick.
#[test]
fn replies_and_pushes_keep_their_order() {
    let presented = Arc::new(AtomicUsize::new(0));
    let (mut backend, helper) = pushing(move |request| {
        let fresh = || {
            let fresh = Record::Fresh {
                presented_ago_us: None,
                presented_count: None,
            };
            (fresh, None)
        };
        match request {
            Request::Video { .. } => {
                let answer = Response {
                    context: Some(desktop()),
                    video: Some(crate::VideoFormat::default()),
                    submitted: Some(false),
                    ..Response::default()
                };
                vec![
                    unit(1, 1, 1),
                    fresh(),
                    (Record::Reply(Box::new(answer)), Some(vec![2])),
                ]
            }
            Request::Context {} => {
                let answer = Response {
                    context: Some(desktop()),
                    cursor: Some(CursorPosition {
                        x: 1,
                        y: 1,
                        visible: true,
                    }),
                    ..Response::default()
                };
                let mut script = vec![pointer(5, 5)];
                script.extend(reply(answer));
                script
            }
            Request::Presented { .. } => {
                let first = presented.fetch_add(1, Ordering::SeqCst) == 0;
                let mut script = if first { vec![] } else { vec![fresh()] };
                script.extend(reply(Response {
                    context: Some(desktop()),
                    ..Response::default()
                }));
                // After the first answer: in before the second.
                if first {
                    script.push(pointer(6, 6));
                }
                script
            }
            _ => reply(Response::default()),
        }
    });
    let desktop = desktop();
    let taken = backend
        .capture_video(&desktop, VideoCodec::H264, false, rate())
        .unwrap();
    assert_eq!(taken.unwrap().bytes, [1]);
    let next = backend.drain_video(&desktop, Duration::ZERO).unwrap();
    assert_eq!(next.unwrap().bytes, [2]);
    // Armed, and the fresh picture told before the answer is spent.
    assert!(!backend.submitted_video());
    assert!(backend.capture_idle_until(None).is_some());
    backend.context().unwrap();
    let at = |backend: &mut RemoteBackend| backend.cursor(&desktop).unwrap().unwrap().x;
    assert_eq!(at(&mut backend), 1);
    backend.presented(&desktop).unwrap();
    assert!(backend.capture_idle_until(None).is_some());
    // The pointer pushed after the first answer, and the fresh picture
    // before the second, are in once the second is.
    backend.presented(&desktop).unwrap();
    assert_eq!(at(&mut backend), 6);
    assert_eq!(backend.capture_idle_until(None), None);
    drop(backend);
    helper.join().unwrap();
}

/// Protocol 12 over a pipe to a worker serving a display whose source dates
/// its pictures: a capture that found nothing new leaves the tick armed,
/// and the worker tells thread C, waking it, once a picture is presented;
/// the capture that takes it dates it by when the display presented it.
#[test]
fn a_fresh_picture_is_told_for_an_armed_tick_and_dated() {
    let probe = Probe::default();
    probe.date(60.0, Instant::now(), Arc::new(Instant::now));
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-fresh");
    let (mut backend, helper) = synthetic::<true>(&probe, journal.to_path_buf(), |_| None, |_| {});
    let waiter = bound(&mut backend);
    let context = backend.context().unwrap();
    backend
        .configure_video(crate::VideoLimits::for_viewer(60.0), 60)
        .unwrap();
    let rate = crate::VideoRate {
        bitrate: crate::qos::ENCODER_BITRATE_MIN,
        pace_fps: 60,
    };
    backend
        .capture_video(&context, VideoCodec::H264, true, rate)
        .unwrap();
    assert!(!backend.submitted_video(), "nothing presented yet");
    assert!(backend.capture_idle_until(None).is_some());
    probe.land(Instant::now());
    woken(&waiter, || backend.capture_idle_until(None).is_none());
    // Its unit is pushed after the answer: the capture may hand it over
    // already, or a drain once it is in.
    let mut unit = backend
        .capture_video(&context, VideoCodec::H264, true, rate)
        .unwrap();
    assert!(backend.submitted_video());
    let (present, _) = probe.takes()[0];
    let picture = backend.submitted_picture().unwrap();
    assert_eq!(picture.presents, 1);
    assert!(
        picture.at >= present && picture.at < present + Duration::from_millis(500),
        "{:?} after the present",
        picture.at.saturating_duration_since(present)
    );
    // Spent: the next tick captures.
    assert_eq!(backend.capture_idle_until(None), None);
    woken(&waiter, || {
        if unit.is_none() {
            unit = backend.drain_video(&context, Duration::ZERO).unwrap();
        }
        unit.is_some()
    });
    assert!(unit.is_some_and(|unit| unit.format.keyframe));
    drop(backend);
    helper.join().unwrap();
}

/// While thread C streams, the worker pushes the pointer as it moves, with
/// no request, and a change wakes thread C, which samples it at once;
/// binding turns the stream on, and pausing every viewer turns it off,
/// pushing nothing, and on again, once per change.
#[test]
fn pointer_pushes_arrive_while_streaming() {
    let probe = Probe::default();
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-pointer");
    let streams = Arc::new(Mutex::new(Vec::new()));
    let seen = streams.clone();
    let (mut backend, helper) = synthetic::<false>(
        &probe,
        journal.to_path_buf(),
        |_| None,
        move |request| {
            if let Request::Stream { on } = request {
                seen.lock().push(*on);
            }
        },
    );
    let display = Probe::display();
    probe.point(display.x + 5, 7);
    let context = backend.context().unwrap();
    let waiter = bound(&mut backend);
    let at = |backend: &mut RemoteBackend| {
        backend
            .cursor(&context)
            .unwrap()
            .map(|cursor| (cursor.x, cursor.y))
    };
    assert_eq!(at(&mut backend), Some((5, 7)));
    probe.point(display.x + 9, 3);
    woken(&waiter, || backend.cursor_changed());
    assert_eq!(at(&mut backend), Some((9, 3)));
    assert!(!backend.cursor_changed(), "read");
    probe.point(display.x + 2, 2);
    woken(&waiter, || at(&mut backend) == Some((2, 2)));
    backend.video_paused(true);
    // Paused: nothing pushed, nothing woken, however far the pointer goes.
    waiter.wait(Instant::now()).unwrap();
    probe.point(display.x + 4, 4);
    let paused = Instant::now() + 4 * crate::worker::CURSOR_INTERVAL;
    assert!(!waiter.wait(paused).unwrap());
    assert_eq!(at(&mut backend), Some((2, 2)));
    backend.video_paused(false);
    backend.video_paused(false);
    woken(&waiter, || at(&mut backend) == Some((4, 4)));
    assert_eq!(*streams.lock(), [true, false, true]);
    drop(backend);
    helper.join().unwrap();
}

/// The worker pushes a reading that differs from the pointer the host
/// holds, which an answer may have set: an input's answer reports the move
/// it applied, and a pointer the system then put back where the worker last
/// pushed it is pushed again.
#[test]
fn a_pointer_back_where_it_was_pushed_is_pushed_after_an_answer() {
    let probe = Probe::default();
    let journal = pb_mapper_core::test_support::TempPath::new("desktop-oop-pointer-back");
    let (mut backend, helper) = synthetic::<false>(&probe, journal.to_path_buf(), |_| None, |_| {});
    let display = Probe::display();
    probe.point(display.x + 5, 7);
    let context = backend.context().unwrap();
    let waiter = bound(&mut backend);
    let at = |backend: &mut RemoteBackend| {
        backend
            .cursor(&context)
            .unwrap()
            .map(|cursor| (cursor.x, cursor.y))
    };
    probe.point(display.x + 9, 3);
    woken(&waiter, || at(&mut backend) == Some((9, 3)));
    backend
        .input_in_context(&context, &Input::Move { x: 20, y: 20 })
        .unwrap();
    assert_eq!(at(&mut backend), Some((20, 20)), "as the input left it");
    // The system stops the move short, where the pointer was.
    probe.point(display.x + 9, 3);
    woken(&waiter, || at(&mut backend) == Some((9, 3)));
    drop(backend);
    helper.join().unwrap();
}

/// A resume captures at its first tick: the worker dropped the arming of
/// the capture before the pause, a keyframe's included, and tells of none.
#[test]
fn a_resume_drops_the_arming_of_the_capture_before_the_pause() {
    let (mut backend, helper) = remote(|request| Response {
        context: Some(desktop()),
        submitted: matches!(request, Request::Video { .. }).then_some(false),
        ..Response::default()
    });
    let _waiter = bound(&mut backend);
    backend
        .capture_video(&desktop(), VideoCodec::H264, true, rate())
        .unwrap();
    let now = Instant::now();
    assert!(
        backend
            .capture_idle_until(Some(now))
            .is_some_and(|until| until > now)
    );
    backend.video_paused(true);
    backend.video_paused(false);
    assert_eq!(backend.capture_idle_until(Some(now)), None);
    drop(backend);
    helper.join().unwrap();
}

/// The same limits again rebuild the worker's encoder, and what it pushed
/// before goes with it; other limits keep it.
#[test]
fn the_same_limits_again_drop_what_the_worker_pushed() {
    let (mut backend, helper) = pushing(|request| {
        let mut script = vec![];
        if matches!(request, Request::Presented { .. }) {
            script.extend([unit(1, 0, 1), unit(1, 0, 2)]);
        }
        script.extend(reply(Response {
            context: Some(desktop()),
            ..Response::default()
        }));
        script
    });
    let desktop = desktop();
    let (sixty, thirty) = (
        crate::VideoLimits::for_viewer(60.0),
        crate::VideoLimits::for_viewer(30.0),
    );
    backend.configure_video(sixty, 60).unwrap();
    backend.presented(&desktop).unwrap();
    assert_eq!(backend.pending_video(), 2);
    backend.configure_video(thirty, 30).unwrap();
    assert_eq!(backend.pending_video(), 2, "other limits keep the encoder");
    backend.configure_video(thirty, 30).unwrap();
    assert_eq!(backend.pending_video(), 0);
    assert!(
        backend
            .drain_video(&desktop, Duration::ZERO)
            .unwrap()
            .is_none()
    );
    drop(backend);
    helper.join().unwrap();
}
