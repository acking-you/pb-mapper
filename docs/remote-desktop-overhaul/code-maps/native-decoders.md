# Map: native video decoders and texture plumbing (Android MediaCodec, Windows Media Foundation)

Repo root below = `<repo>` (branch
`claude/remote-desktop-overhaul`, head `a41eaa7`). All paths are relative to it. Line numbers
were read from the files as they are at that head.

## 1. Overview

- Native H.264 decode into a Flutter external texture exists on **Android and Windows only**:
  `DesktopVideoTexture.supported => Platform.isAndroid || Platform.isWindows`
  (`ui/lib/src/ffi/desktop_video.dart:42`). The viewer asks for H.264 only on those two
  platforms (`'native_video': Platform.isWindows || Platform.isAndroid`,
  `ui/lib/src/ffi/desktop_api.dart:439`). On every other platform the host sends JPEG, which Dart
  decodes with `ui.instantiateImageCodecWithSize` (`ui/lib/src/pages/desktop_viewer.dart:111-165`).
- Both native implementations expose the **same MethodChannel**, `pb_mapper/desktop_video`, with the
  same methods (`create`, `submit`, `reset`, `dispose`, `stats`) and the same native→Dart `event`.
  Android: `ui/android/app/src/main/kotlin/com/pb_mapper/ui/DesktopVideo.kt`. Windows:
  `ui/windows/runner/desktop_video.cpp`.
- Today **every compressed access unit crosses Dart**:
  Rust receive task → `media::Queue` → Dart frame worker isolate (blocking FFI
  `pb_mapper_desktop_wait_frame`) → main isolate (`DesktopStore._acceptFrame`) → MethodChannel
  `submit` → platform main/UI thread → native decoder thread. The `submit` reply comes back only
  after the decoder has accepted (Android) or fed (Windows) that unit, and Dart awaits it before
  it asks for the next frame. That makes the whole chain stop-and-wait with exactly one frame in
  flight (details in 3.2 and 7, P1-1).
- **Presentation receipts** come from the texture consumer, not from the decoder. On Android it
  is Flutter's `OnFrameConsumedListener` after `updateTexImage`, matched by exact timestamp. On
  Windows it is the raster thread's first `AcquireSurface` of a given frame. They flow back
  native → platform thread → Dart `event` → `DesktopStore._presentVideo` → 33 ms coalesced
  `api.presented` → FFI → `Viewer::presented` → wire `Command::Presented`. They gate both host
  sending credit (today) and input authorisation (must remain).
- No JNI exists anywhere in the project. `libpb_mapper_ffi.so` is only `dlopen`ed by Dart.
  `jni`/`jni-sys`/`ndk-context` are in `Cargo.lock` only as Android-only transitive dependencies
  (section 7, "Direct handoff").

## 2. Components and responsibilities

### 2.1 Android

| File | Responsibility |
| --- | --- |
| `ui/android/app/src/main/kotlin/com/pb_mapper/ui/MainActivity.kt` | `configureFlutterEngine` constructs `DesktopVideo(flutterEngine.dartExecutor.binaryMessenger, flutterEngine.renderer)` (`:14`), where `renderer` is the `TextureRegistry`. `cleanUpFlutterEngine` calls `desktopVideo?.dispose()` (`:20-21`). Also hosts `UpdateInstaller` (unrelated). |
| `.../DesktopVideo.kt` | The channel handler (`:27-56`) and a map `viewers: MutableMap<Long, Viewer>` (`:25`, at most 2 entries, `:31`). The inner class `Viewer` (`:115-547`) owns one `SurfaceTextureEntry`, one `Surface`, one `HandlerThread("pb-desktop-video")` (`:127-128`) and at most one `MediaCodec`. It covers the decode pipeline, the presentation receipts, recovery and diagnostics. The companion has `MAX_BYTES = 4 MiB` (`:550`) and `parameterSets()`, an Annex-B SPS/PPS extractor (`:553-572`). |
| `.../VideoPresentationQueue.kt` | Owned by the worker. A FIFO of decoded output buffers (`Output(index, ptsUs, revision, frameId, readyNs)`, `:7-16`). `take(revision, surfaceBusy)` (`:29-43`) returns either one buffer to release, with `render = current && !superseded`, or `null` while it keeps the newest current picture for a busy surface. `clear()` (`:45-47`). |
| `ui/android/app/src/test/kotlin/com/pb_mapper/ui/VideoPresentationQueueTest.kt` | JVM unit tests for `VideoPresentationQueue` only (section 6). |
| `ui/android/app/src/main/AndroidManifest.xml` | `EnableImpeller=false` (`:14-21`, comment: Impeller's Vulkan bridge copies and resizes the SurfaceTexture on every paint, OpenGL keeps true consumption receipts). The Activity has `configChanges="orientation|keyboardHidden|keyboard|screenSize|smallestScreenSize|locale|layoutDirection|fontScale|screenLayout|density|uiMode"` (`:28`), so rotation and keyboard changes do not recreate the Activity, engine or textures. Also `hardwareAccelerated="true"` (`:29`), `windowSoftInputMode="adjustResize"` (`:30`), `launchMode="singleTop"`. Permissions are INTERNET, REQUEST_INSTALL_PACKAGES, ACCESS_NETWORK_STATE and ACCESS_WIFI_STATE. There is **no CAMERA** permission yet (needed for QR scan, req. 2). |
| `ui/android/app/build.gradle.kts` | `minSdk = flutter.minSdkVersion` (`:38`), which is **24** in Flutter 3.44.9 (`<flutter>/packages/flutter_tools/gradle/src/main/kotlin/FlutterExtension.kt:26`). compile/targetSdk are 36 and the NDK is `28.2.13676358` (same file `:23,34,42`). Java/Kotlin target 11 (`:18-25`). `jniLibs.srcDirs("../../native/android")` (`:27-31`), i.e. `ui/native/android/<abi>/libpb_mapper_ffi.so`. Dependencies: only `testImplementation("junit:junit:4.13.2")` (`:90-92`); no AndroidX media, CameraX or ML Kit. Signing pinned (`:51-84`). |
| `ui/android/settings.gradle.kts` | AGP 8.9.1, Kotlin 2.1.0 (`:22-23`). Gradle wrapper 8.12. |
| `ui/android/gradle.properties` | `android.useAndroidX=true`, Jetifier on. |

The Android native library is built and loaded like this:

- Local build: `scripts/build/pb-mapper-ffi-android.sh` runs
  `cargo ndk -o ui/native/android -t armeabi-v7a -t arm64-v8a -t x86_64 build -p pb-mapper-ffi --release`.
  The Makefile target is `build-pb-mapper-ffi-android`.
- CI (`.github/workflows/release-ui.yml:396-402`) uses cargo-ndk 4.1.2 and NDK 28.2.13676358 with
  `--locked`, then `flutter build apk --split-per-abi`. Lines `:443-448` check that the packaged
  `lib/<abi>/libpb_mapper_ffi.so` is byte-equal to the stripped build.
- `ui/native/**/libpb_mapper_ffi.*` is gitignored (`.gitignore:10`).
- Crate: `ui/native/pb_mapper_ffi/Cargo.toml`, `crate-type = ["cdylib", "staticlib"]`. It has no
  `jni`/`ndk-context` dependency and no `[target.'cfg(target_os="android")']` section. Lints are
  workspace-wide, so `unwrap_used`/`expect_used` are denied.
- Loading: Dart `DynamicLibrary.open('libpb_mapper_ffi.so')` (`ui/lib/src/ffi/pb_mapper_ffi.dart:243-244`).
  Kotlin never calls `System.loadLibrary("pb_mapper_ffi")`. The repo has no `JNI_OnLoad` and no
  `Java_*` exports, and nothing calls `ndk_context::initialize_android_context`. A grep of `*.rs`
  and `*.toml` finds none.

### 2.2 Windows

| File | Responsibility |
| --- | --- |
| `ui/windows/runner/flutter_window.cpp/.h` | After `RegisterPlugins`, it creates `desktop_video_ = std::make_unique<DesktopVideo>(engine, GetHandle())` (`.cpp:28`). `OnDestroy` resets it before the controller (`:44`). `MessageHandler` first offers each message to `desktop_video_->HandleMessage(message)` (`:56`). This is how decoder threads marshal replies and events onto the platform thread, through a `PostMessage(kWake)`. |
| `ui/windows/runner/desktop_video.h` | Pimpl `DesktopVideo { DesktopVideo(FlutterEngine*, HWND); bool HandleMessage(UINT); }`. |
| `ui/windows/runner/desktop_video.cpp` | `Impl` (`:526-599`) holds the channel, the registrar and `viewers` (≤2). `Events` (`:65-87`) coalesces events and replies to the platform thread. `Viewer` (`:111-523`) owns the texture registration, the `queue_` of submissions, one decoder `std::thread worker_`, `latest_` (the newest decoded `Surface`) and `borrowed_` (the last lease). `Decoder` (`:248-453`) wraps a D3D11 device on Flutter's adapter, a DXGI device manager, `CLSID_CMSH264DecoderMFT` (DXVA), and a D3D11 video processor that converts NV12 to a BGRA shared texture. |
| `ui/windows/runner/CMakeLists.txt` | Adds `desktop_video.cpp` (`:11`). Links `d3d11 dxgi mfplat mf mfuuid wmcodecdspuuid ole32 oleaut32` (`:38`). It does **not** link `pb_mapper_ffi`. |
| `ui/windows/CMakeLists.txt` | Standard settings `/W4 /WX`, `_HAS_EXCEPTIONS=0`, `/EHsc`. Installs `../native/windows/x64/pb_mapper_ffi.dll` and `pb-mapper-desktop-helper.exe` next to the exe only `if(EXISTS ...)`. |
| `ui/windows/runner/main.cpp` | `CoInitializeEx(COINIT_APARTMENTTHREADED)` on the platform thread. The decoder thread separately does `CoInitializeEx(COINIT_MULTITHREADED)` + `MFStartup(MF_VERSION, MFSTARTUP_LITE)` (`desktop_video.cpp:456-457`). |

The DLL is built by `scripts/build/pb-mapper-ffi-windows.ps1` (`cargo build -p pb-mapper-ffi --release`
→ `ui/native/windows/x64/`, also copied into existing `ui/build/windows/x64/runner/{Release,Debug,Profile}`).
Dart loads it with `DynamicLibrary.open('pb_mapper_ffi.dll')` (`pb_mapper_ffi.dart:255-256`).

### 2.3 Dart and Rust callers (the other side of the contract)

- `ui/lib/src/ffi/desktop_video.dart`: `DesktopVideoEvent` (`:7-33`) and `DesktopVideoTexture`
  (`:37-118`). There is one static channel handler for `event` (`:53-64`); events for unknown or
  disposed ids are dropped. `submit` sends `ptsUs`, which **both natives ignore** (`:72-92`).
  `stats()` sends only `textureId` (`:100-106`).
- `ui/lib/src/state/desktop_store.dart`: `_acceptFrame` (`:471-560`), `_presentVideo`
  (`:562-605`), `_flushFeedback` (`:607-647`), `_consumeFrames` (`:660-686`), `_clearVideo`
  (`:688-726`), `_openViewer` (`:893-950`), `fresh` (`:125-135`).
- `ui/lib/src/ffi/pb_mapper_service.dart`: `_desktopFrameIsolate` (`:27-53`),
  `_desktopFramesWorker` (`:57-114`, a single `credit`), `DesktopFrameStream` (`:119-246`), and
  `desktopCall` (`:288-301`). The latter routes `presented|keyframe|input|control|release` into the
  frame worker isolate's mailbox.
- `ui/native/pb_mapper_ffi/src/desktop.rs`: `DesktopState::wait_frame` (`:167-190`, bounded
  16 ms), `pb_mapper_desktop_wait_frame` (`:1244-1255`), `export_frame` (`:1257-1278`),
  `pb_mapper_desktop_free_frame` (`:1280-1292`), `Request::Connect { native_video, video }`
  (`:718-770`, `native_video` → `Viewer::connect_with_presentation_feedback` with
  `[H264, Jpeg]`), and `Request::Keyframe`/`Request::Presented` (`:790-814`).
- `crates/pb-mapper-desktop/src/viewer.rs`: receive loop (`:212-292`), send loop (`:293-321`),
  `presented` (`:366-368`), `next_frame` (`:377-379`), `request_keyframe` (`:381-383`).
  `crates/pb-mapper-desktop/src/media.rs`: `MAX_QUEUED_FRAMES = 8` and `MAX_QUEUED_BYTES = 8 MiB`
  (`:10-11`), `push` (`:30-69`), `pop` (`:70-82`, a status-only entry first), `push_status`
  (`:85-93`). `wire::MAX_FRAME = 4 MiB` (`crates/pb-mapper-desktop/src/wire.rs:6`) matches both
  natives' `MAX_BYTES`/`kMaxBytes`.

## 3. Data and control flow

### 3.1 Session start and texture creation

1. `DesktopStore._openViewer` (`desktop_store.dart:893-950`) calls `_clearVideo()`, which disposes
   any previous texture. It then calls `api.connect(...)`. That sends `native_video: true` and
   `video: {max_width:1920,max_height:1080,max_fps:<refresh clamp 30..90 on Android, 90 otherwise>,bitrate:12000000}`
   (`desktop_api.dart:428-447`). Finally it starts `_consumeFrames(StreamIterator(api.frames()))`
   (`:926-928`).
2. The first H.264 frame with bytes reaches `_acceptFrame`. If there is no texture yet,
   `DesktopVideoTexture.create()` installs the static `event` handler once and invokes `create`
   (`desktop_video.dart:49-70`).
   - Android `create` (`DesktopVideo.kt:30-35`): `check(viewers.size < 2)`, then
     `Viewer(textures.createSurfaceTexture())`, whose `id = entry.id()`. The `Viewer` init
     registers `entry.setOnFrameConsumedListener` (`:165-178`) and starts the HandlerThread. The
     codec is created lazily on the first keyframe.
   - Windows `create` (`desktop_video.cpp:537-546`): the same limit of 2.
     `engine->GetGraphicsAdapter(&adapter)` runs, then `Viewer::Start()` (`:115-126`) registers a
     `flutter::GpuSurfaceTexture(kFlutterDesktopGpuSurfaceTypeDxgiSharedHandle, callback)`, gets
     `id_` and starts `worker_ = std::thread(Run)`. The decoder is created lazily on the first key.
3. Dart subscribes to `texture.events` and filters by epoch and identity (`desktop_store.dart:517-529`).

### 3.2 Per-frame path, Android (today)

1. **Rust tokio receive task** (`viewer.rs:216-291`): `read_json::<SessionStatus>` +
   `read_bytes(MAX_FRAME)` → `incoming.push(frame.clone())` (`media::Queue`, bounded 8 frames /
   8 MiB; on overflow or a new generation it drops and waits for a keyframe, setting the keyframe
   flag, `media.rs:33-62`) → if `take_keyframe_request()`, `recover.send_modify` (→
   `Command::Keyframe` on the send task, `viewer.rs:305`) → `frames_tx.send_replace(Some(frame))`
   (a wake-up).
2. **Dart frame worker isolate** (`pb_mapper_service.dart:57-114`): this runs only while it holds
   `credit`. It calls `pb_mapper_desktop_wait_frame`, which `block_on`s
   `DesktopState::wait_frame`: `next_frame()` or wait up to **16 ms** on the watch
   (`desktop.rs:167-190`). The buffer is `u32 LE metadata length | JSON SessionStatus | bytes`
   (`desktop.rs:1257-1278`). Dart copies the bytes (`Uint8List.fromList`, `pb_mapper_service.dart:48`),
   frees the native buffer, sets `credit=false` and `SendPort.send`s the map to the main isolate
   (another copy).
3. **Main isolate**: `DesktopFrameStream` adds it to the stream (`:204-209`). `_consumeFrames`
   (`desktop_store.dart:665-668`) awaits `_acceptFrame(frame)`. For H.264 with bytes it validates
   the size (`:514-516`), awaits `texture.reset()` on a generation change (`:531-539`), records
   `_videoStatuses[frameId] = (status, _frameAt)` (bounded 256, `:540-543`), then **awaits**
   `texture.submit(...)` (`:544-552`). StandardMethodCodec encodes the `Uint8List`, another copy.
4. **Android main (platform) thread**: the handler validates bounds (`DesktopVideo.kt:40-43`) and
   calls `viewer.submit(Packet(...), result)` (`:44`). `Viewer.submit` (`:217-252`) does the
   following under `lock`:
   - a generation older than `acceptedGeneration` → reply at once (`:221`);
   - a new generation → `revision++`, `clearQueue()`, `awaitingKey=true` (`:222-228`);
   - `frameId <= lastAccepted` → reply (`:229`);
   - `queue.size >= 4` → drop all (`dropped += size`), `revision++`, `awaitingKey`, retry flag
     (`:231-237`);
   - a non-key while `awaitingKey` → `dropped++` and a needKeyframe event (`:238-241`);
   - otherwise enqueue `Submission(packet.copy(revision=current), reply, receivedNs)` (`:243`) and
     **hold the reply**.

   It then calls `schedulePump(0)` (`:248`), which posts `pumpTask` to the worker
   (`:429-439`, coalesced by `pumpPosted`/`pumpDueMs`).
5. **Worker HandlerThread "pb-desktop-video"**: `pump()` (`:469-546`):
   - `renderLatest(codec)` first (`:441-462`).
   - Then up to 4 attempts. A stale-revision input is retired and completed (`:480-484`). It calls
     `configure(packet)` if there is no codec, the revision changed or the size changed (`:485-488`).
   - It stops if `pending.size >= 16 || waitingOutputs + outputs.size >= 4` (`:492`) or if no input
     index is available (`inputs` filled by `onInputBufferAvailable`, `:386-390`).
   - Otherwise it copies the bytes into the codec input buffer (`:504-507`), sets
     `pts += 16667` (a local unique timestamp in µs; the network pts is never used, `:508`) and
     records `pending[pts] = PendingFrame(packet without bytes, receivedNs, queuedNs)` (`:510`).
   - It calls `queueInputBuffer(index,0,len,pts,0)` (`:511`), then `waitingOutputs++`. In
     `finally` it calls `input.complete()` → `main.post { reply.success(null) }` (`:66-71`,
     `:518-522`).
6. **Android main thread → Dart main isolate**: the `submit` future completes → `_acceptFrame`
   returns → `_consumeFrames` calls `frames.moveNext()` → the stream resumes →
   `scheduleMicrotask(_credit)` → `_requests.send(true)` → the worker isolate takes its next
   `wait_frame` (`pb_mapper_service.dart:122-128,180-184`).
7. **Decoder output callbacks**, on the worker thread because `setCallback(..., worker)` was used
   (`:385-423`). `onOutputBufferAvailable` (`:392-416`) looks up `pending[info.presentationTimeUs]`.
   It renders only if the frame is known, the revision is current and the buffer is not a
   `BUFFER_FLAG_CODEC_CONFIG` buffer. In that case it calls `outputs.enqueue(...)` and
   `decoded++`; otherwise it releases without rendering. Then `pump()`.
8. **`renderLatest`** (`:441-462`): it loops on `outputs.take(revision, surfacePtsUs != null)`.
   Superseded or stale outputs are released with `render=false` (`dropped++`, `pending.remove`).
   The newest current one is released with `releaseOutputBuffer(index, true)` only when **no
   output is waiting on the surface**. It sets `surfacePtsUs = output.ptsUs` and
   `surfaceReleasedAtMs`. **At most one picture is in flight to the SurfaceTexture.**
9. **Flutter raster thread**: `updateTexImage` → `OnFrameConsumedListener` (`:168-177`) reads
   `entry.surfaceTexture().timestamp / 1000`. If it equals the last consumed timestamp (a repaint)
   it is ignored. Otherwise it stores `consumedTimestamp`/`consumedAtNs` and posts `consumeTask`
   once (`consumePosted` CAS).
10. **Worker `consumeSurface`** (`:180-215`): only when `consumedTimestamp == surfacePtsUs`. It
    sets `surfacePtsUs = null`, removes the pending entry, and if the revision is current records
    `lastPresented*`, `presented++` and `event(packet, consumed=true, key=false)`. It records the
    timings when diagnostics are on. The loop re-checks the flag so a concurrent callback is never
    lost, then calls `pump()` directly on the same turn (`:206-212`).
11. **Stall detection** at the end of `pump` (`:524-539`): a released picture not consumed for
    more than 2000 ms → `fail("Native video texture was not consumed")`. Outputs owed for more
    than 2000 ms without progress → `fail("Native video decoder stalled")`. Timers are scheduled
    only for these checks (`schedulePump(delay)`). There is no idle polling.

Effective depth: Dart stop-and-wait means **at most one** submit is outstanding per texture. The
Kotlin `queue.size >= 4` overflow branch (`:231-237`) is therefore unreachable with the current
caller. The codec holds at most 4 inputs that are decoding or decoded-awaiting-release, plus 1 on
the surface.

### 3.3 Per-frame path, Windows (today)

Steps 1–3 are the same as on Android. Then:

4. **Platform (Win32 UI) thread**: the handler (`desktop_video.cpp:553-566`) validates the bounds,
   then **copies** `*data` into `packet.bytes` (`:560`, after the codec already copied it into an
   `EncodableValue` vector). It calls `viewer->Submit(packet, std::move(result))`.
   `Viewer::Submit` (`:144-180`) mirrors Kotlin, all under `mutex_`:
   - an older generation → `Success` (`:148`);
   - a new generation → `++revision_`, `ClearQueue`, `latest_.reset()`, `awaiting_key_` (`:149-156`);
   - a duplicate or older frame → `Success` (`:157`);
   - `queue_.size() >= 4` → drop all, `++revision_`, request a key (`:159-166`);
   - a non-key while awaiting → `++dropped_` + key event (`:168-170`);
   - otherwise `queue_.push_back({packet, reply})` + `notify` (`:172-175`), **holding the reply**.
5. **Decoder thread** `Run()` (`:455-504`):
   - It waits on `wake_`. A revision mismatch with the live decoder → `decoder->Flush(revision_)`
     (`:466-468`; flush, not recreate).
   - It pops one submission and skips it if stale (`:476`, the reply is completed).
   - It creates a new `Decoder` when there is none or the size changed; that **requires a key**,
     else it throws `"A fresh key frame is required"` (`:477-480`).
   - `decoder->Feed(packet)`: `MFCreateMemoryBuffer` + memcpy, `next_pts += 166667` (100 ns
     units), `MFSampleExtension_CleanPoint` on a key, `ProcessInput`, and
     `pending.emplace(pts, metadata)`. Throws if `pending.size() >= 16` (`:379-399`).
   - `events_->Complete(reply)`: the reply is released right after `ProcessInput` (`:483`) and is
     **not** gated by output or presentation (unlike Android).
   - It drains up to 16 outputs through `Take()` (`:400-452`): `ProcessOutput`;
     `MF_E_TRANSFORM_STREAM_CHANGE` → `SetOutput()` (NV12 type, aperture validation `:335-378`);
     the metadata is found by sample time and older entries are erased (`:413-416`). It
     `CreateVideoProcessorInputView` on the DXVA surface, **allocates a new BGRA
     `D3D11_RESOURCE_MISC_SHARED` texture for every decoded frame** (`:433-441`),
     `VideoProcessorBlt` (BT.709 limited → full), `context->Flush()`, and returns
     `shared_ptr<Surface>` (the legacy `GetSharedHandle`, `:93-104`).
   - Each surface → `latest_ = surface` under the mutex if still current, then
     `registrar_->MarkTextureFrameAvailable(id_)` (`:484-494`). Superseded decoded surfaces are
     simply replaced in `latest_` and are not counted as dropped.
   - On an exception it completes the reply, `decoder.reset()`, `Fail(packet, e.what())`
     (`:495-499`).
6. **Platform thread `Drain()`** (`:583-598`), triggered by `PostMessage(window, kWake)` from
   `Events::Send/Complete` (`:72-86`) and dispatched by `FlutterWindow::MessageHandler`
   (`flutter_window.cpp:56`): it replies to all completed submits, then sends the coalesced events
   whose `Current(revision)` holds.
7. **Raster thread** texture callback → `AcquireSurface()` (`:196-222`). It returns `nullptr` when
   there is no `latest_` or it is stale; Flutter then paints nothing for the texture. Otherwise it
   leases `latest_` (a `SurfaceLease` freed by Flutter's `release_callback`, `:211-216`; the last
   lease is kept in `borrowed_`, `:219`). On the **first acquisition of a new
   (revision, frame)** it does `++presented_` and `events_->Send(presented)` (`:203-207,220`).

### 3.4 Presentation receipt flow (both platforms) → host

Native `event {presented:true}` → platform thread → `channel.invokeMethod("event", map)`
(Android `DesktopVideo.kt:324-335` checks `!closed && frame.revision == revision.get()`; Windows
`Drain` checks `Current`) → Dart static handler → `texture._events` → `_presentVideo`
(`desktop_store.dart:562-605`).

`_presentVideo` accepts the receipt only if all of these hold:

- `event.generation == _videoGeneration`;
- `_videoStatuses[frameId]` is known and its generation equals the current `frame.status.generation`;
- the frameId is greater than the last presented one;
- the viewer is streaming;
- the frame was **received by Dart** less than 2 s ago (`:577-582`).

It then sets `videoPresented`/`_videoPresentedAt`, the present-fps sample, and
`_pendingPresentation`. `_flushFeedback` (`:607-647`) coalesces to one ACK per 33 ms, plus a
trailing timer. It calls `api.presented(gen, frameId)` → `desktopCall` → the frame worker isolate
mailbox (`pb_mapper_service.dart:288-301`), which may sit behind a blocking 16 ms `wait_frame` →
`pb_mapper_desktop_call_json` → `Viewer::presented` (a watch, `viewer.rs:366-368`) → the send
task writes `Command::Presented` (`viewer.rs:312`). The host uses it for congestion credit (today)
and for input authorisation: `release_if_stale` requires the presented frame to be in `recent`
and younger than 2 s (`crates/pb-mapper-desktop/src/worker.rs:402-414`).

### 3.5 Keyframe and recovery flows

- **Native needKeyframe events** are sent in these cases:
  - Android: dropped non-key while awaiting a key (`DesktopVideo.kt:238-241`); queue overflow
    (`:231-237`); `fail()` (`:351-362`), which is called from codec errors (`:420-422`), exceptions
    in pump/configure (`:540-542`), and the surface/decoder stall checks (`:525-531`). Messages are
    `"Native H.264 decoding unavailable"`, `"Native video texture was not consumed"` and
    `"Native video decoder stalled"`.
  - Windows: awaiting-key drop (`:168-170`), overflow (`:159-166`), and `Fail` with the exception
    text (`:234-246`), for example `"A fresh key frame is required"`,
    `"Native decoder backlog"`, or an HRESULT string.
- **Dart** `_presentVideo` sets `error = event.message` if it is non-empty (`desktop_store.dart:565-574`).
  A routine recovery such as a size change without an IDR is therefore shown as a user error.
  It then sets `_pendingKeyframe` → `api.keyframe()` → `Viewer::request_keyframe` → watch →
  `Command::Keyframe`.
- **Dart watchdog**: when not `fresh` for ≥700 ms it requests a keyframe at most once per second,
  and after ≥3 s it reconnects (`desktop_store.dart:413-425`).
- **Rust media queue**: on overflow or generation switch it drops P-frames until an IDR and sets the
  request flag (`media.rs:47-62`) → the receive loop wakes `recover` (`viewer.rs:287-289`).
- The **IDR content requirement on Android**: `configure()` throws unless `packet.keyframe`
  (`:369`), and `parameterSets()` requires both SPS (NAL type 7) and PPS (type 8) inside the
  access unit (`:570`). They become `csd-0`/`csd-1` (`:380`). The key AU itself is fed as normal
  input with flags 0.

### 3.6 Reset and generation change

- Dart calls `texture.reset()` on a generation change before submitting that generation's IDR
  (`desktop_store.dart:531-539`), and when the codec switches away from H.264 (`:504-512`).
- **Android `reset`** (`:281-302`) does `revision++`, `clearQueue` (completing held replies),
  `awaitingKey`, `acceptedGeneration=-1` and `lastAccepted=0` under the lock. It then posts to the
  worker: `releaseCodec()` only if the active codec's revision is older than the reset revision
  (`:295-297`), then `pump()`, then `main.post { reply.success }`. That last reply is a **barrier**
  Dart awaits.
- **Windows `Reset`** (`:134-143`) does the same under the mutex, plus `latest_.reset()`, and
  replies immediately. The worker flushes the decoder lazily (`:466-468`).
- **Android rebuilds the codec on every revision change**, not only on a new generation but also
  after an overflow, a reset or a failure: `pump` → `configure` → `releaseCodec` +
  `createDecoderByType` (`:485-488,364-427`). The comment at `:365-367` gives the reason: an async
  flush leaves callbacks with invalid indices. Windows flushes instead.

### 3.7 Size changes

- Android: `configure` runs when `current.width/height` differ (`:486`). It calls
  `surfaceTexture().setDefaultBufferSize(w,h)` (`:370`) and creates a new `MediaFormat` with
  `KEY_MAX_INPUT_SIZE=4 MiB`, `KEY_MAX_WIDTH/HEIGHT=packet size`, `KEY_LOW_LATENCY=1` (API ≥ 30)
  and `KEY_ALLOW_FRAME_DROP=0` (API ≥ 29) (`:371-380`). The decoder is
  `MediaCodec.createDecoderByType("video/avc")`, the platform's first AVC decoder (`:381`). Its name
  is reported as `decoder` in stats. A P-frame at a new size → failure → recovery (3.5).
- Windows: a new `Decoder` when the size differs (`:477-480`). The output aperture must equal the
  authorised frame size; only macroblock padding may be cropped (`:343-369`).
- Dart: `_videoGeometry`/aspect comes from the latest **received** status
  (`desktop_viewer.dart:71-76`). The `Texture` widget and `textureId` stay the same. The native
  texture's pixel size equals the encoded size. The widget size is never sent to native code: the
  Windows callback ignores its `(size_t, size_t)` arguments (`desktop_video.cpp:119`), and Android
  sizes the buffer only from the packet.

### 3.8 Dispose and teardown

- Dart `_clearVideo` (`desktop_store.dart:688-726`) increments the epoch, cancels the frames
  iterator (which stops the worker isolate) and disposes the texture. That runs on every
  `_openViewer` (reconnect) and `disconnect`, so a reconnect gets a new texture and decoder.
- **Android `dispose`** (`:304-315`): `closed=true`, `revision++`, `clearQueue` (completing
  replies), remove the consumed listener. On the worker: `releaseCodec`, `surface.release()`,
  `main.post { entry.release() }`, `thread.quitSafely()`. `DesktopVideo.dispose()`, called from
  `MainActivity.cleanUpFlutterEngine`, clears the handler and disposes all viewers (`:58-62`).
- **Windows `dispose`** (`:568`): `viewers.erase`, then `Viewer::Close()`, which calls
  `StopWorker` (`stopped_`, `++revision_`, `ClearQueue`, join, `:223-233`), then
  `UnregisterTexture(id_, [keep_alive]{})`. `~Impl` (`:574-582`) closes all viewers, `Drain()`s
  every pending reply while the messenger exists, then deactivates `Events`.

### 3.9 Threads (today)

| Thread | Android | Windows |
| --- | --- | --- |
| Channel handler, replies, events | Android main thread (no `TaskQueue`; `MethodChannel(messenger, "pb_mapper/desktop_video")`, `DesktopVideo.kt:24`) | Win32 platform thread (handler, plus `Drain` via `WM_APP+0x413`) |
| Decode | One `HandlerThread("pb-desktop-video")` per viewer at default priority. Codec callbacks, `pump`, `consumeSurface` and `stats` all run here. | One `std::thread` per viewer (`Run`, COM MTA) |
| Presentation receipt source | Flutter raster thread (`OnFrameConsumedListener`) | Flutter raster thread (`AcquireSurface`) |
| Shared state | `lock` guards `queue`, `pendingEvents`, `eventPosted`, `pumpPosted`, `acceptedGeneration`, `lastAccepted`, `awaitingKey`. Atomics hold counters, `revision`, `consumedTimestamp`. Everything else is worker-only (`pending`, `inputs`, `outputs`, `codec`, `active`, `surfacePtsUs`). | `mutex_` guards `queue_`, `latest_`, `borrowed_`, `generation_`, `last_`, `awaiting_key_`, consumed ids. Atomics hold counters, `revision_`, `stopped_`. `Events::mutex` guards events and replies. |

## 4. Contracts

### 4.1 MethodChannel `pb_mapper/desktop_video` (StandardMethodCodec)

| Method | Dart args (`desktop_video.dart`) | Android | Windows | Returns |
| --- | --- | --- | --- | --- |
| `create` | none | `viewers.size < 2`, else error "Desktop texture limit reached" | same (`:538`) | `int` textureId |
| `submit` | `textureId:int, bytes:Uint8List, generation:int, frameId:int, ptsUs:int (ignored), width:int, height:int, keyframe:bool` | `bytes` 1..4 MiB, `width` 2..7680, `height` 2..4320, `w*h ≤ 16,777,216`, else "Invalid video bounds" (`:43`). `generation`/`frameId` are read with `!!`, so a missing value raises an NPE, which is reported as an error. | same bounds (`:558`). `Number()` accepts int32/int64 and errors on missing fields. | `null` once the input is accepted by the codec or retired (Android), or after `ProcessInput` or retirement (Windows). Errors otherwise. |
| `reset` | `textureId` | barrier after worker cleanup (`:281-302`) | immediate (`:567`) | `null` |
| `dispose` | `textureId` | removes and disposes (`:47`) | erase + `Close` (`:568`) | `null` |
| `stats` | `textureId` (Dart never sends `timings`) | `timings` defaults to on (`call.argument<Boolean>("timings") != false`, `:48`). The first call allocates `Diagnostics`; `timings:false` frees it. | ignores `timings` | map, see 4.2 |
| unknown | | `notImplemented` | `NotImplemented` | |

Native → Dart `event` (Android `:333`, Windows `:592-596`):
`{textureId:int, generation:int, frameId:int, width:int, height:int, presented:bool, needKeyframe:bool, message:String}`.
Exactly one of `presented` and `needKeyframe` is true. Events are coalesced per texture: one
pending presented event and one pending key event at a time, the latest wins (Android
`pendingEvents: Map<Boolean, …>` `:160,317-336`; Windows map keyed by `(texture, key)` `:70,76`).
Errors use code `"desktop_video"` with the exception message (Android `:52-54`, Windows `:571`).

### 4.2 Stats maps

- **Android** (`DesktopVideo.kt:258-270`): `submitted, submittedKeyframes, decoded, presented,
  lastPresentedGeneration, lastPresentedFrameId, dropped, decoder (codec name), configurations,
  recoveries, lastRecovery, queued, decoding (waitingOutputs), outputs, surfacePending,
  timings?`.
  - `timings` (`:100-112`) is `{elapsed_ms, input_wait, decode_callback, ready_to_release,
    receive_to_present, surface_consume, consume_dispatch, pump_dispatch, pump_work, peak_pending,
    peak_outputs}`. Each timing is `{samples, mean_ms, max_ms}` (`:85`) and is cumulative since
    diagnostics were allocated.
  - `receive_to_present` runs from the **Kotlin main-thread submit receipt** (`receivedNs`,
    `:243`) to SurfaceTexture consumption (`:195`). It excludes Rust queueing, the isolate hop and
    channel time.
  - `dropped` counts queue-overflow, awaiting-key and superseded-decoded drops (`:232,239,460`).
- **Windows** (`desktop_video.cpp:181-187`): `submitted, decoded, presented, dropped, decoder`.
  `decoder` is the fixed string "Media Foundation / D3D11". There are no timings, and `dropped`
  excludes superseded decoded surfaces.
- Consumer: only `ui/tool/desktop_viewer_acceptance.dart:85` polls `stats()` (1 Hz). It derives
  `native_presented_fps` from the `presented` delta (`:121-139`). The app UI does not use stats
  today.

### 4.3 FFI frame buffer that feeds `submit`

`DesktopBuffer {data:*const u8, len:usize}` (`desktop.rs:1214-1218`). The payload is
`u32 LE metadata length | serde_json(SessionStatus) | encoded bytes`. Free every non-null buffer
with `pb_mapper_desktop_free_frame`. The `_after`/`_generation` arguments are ignored
(`:1257-1261`). Fields used for decode are `status.generation`, `status.frame_id`,
`status.video.{codec,width,height,keyframe,pts_us}` (`crates/pb-mapper-desktop/src/lib.rs:125-165,210-219`).
A status-only update arrives as an entry with **empty bytes** in the same stream (`media.rs:72-77`)
and must not touch the decoder (`desktop_store.dart:474-489`).

### 4.4 Constants and bounds

| Bound | Android | Windows | Rust |
| --- | --- | --- | --- |
| Max AU bytes | `MAX_BYTES` 4 MiB (`:550`) | `kMaxBytes` 4 MiB (`:31`) | `wire::MAX_FRAME` 4 MiB |
| Submission queue | 4, then drop all + IDR (`:231`) | 4 (`:159`) | `media` 8 frames / 8 MiB |
| Pending metadata | 16, then input pauses (`:492`) | 16, then throws (`:380`) | n/a |
| Decoding + decoded | ≤ 4 (`:492`) | drains ≤ 16 per input (`:484`) | n/a |
| On surface | 1 (`surfacePtsUs`) | `latest_` only | n/a |
| Stall timeouts | 2000 ms consume, 2000 ms decode (`:525-531`) | none | Dart 700 ms keyframe, 3 s reconnect |
| Textures per process | 2 | 2 | n/a |
| Decoder pts step | 16667 µs | 166667 × 100 ns | n/a |
| Max decode size | 7680×4320, ≤ 16.7 MPix | same | `VideoPreferences` ≤ 3840×2160; UI asks for 1920×1080 |

## 5. Invariants and security boundaries

1. **"Presented" means consumed by the compositor, never queued or decoded.**
   - Android: the exact `SurfaceTexture.timestamp` of the single released output, read in
     `OnFrameConsumedListener` (`DesktopVideo.kt:166-177,180-203`). Repaints of the same timestamp
     return no credit (`:171-173`). `KEY_ALLOW_FRAME_DROP=0` keeps the driver from silently
     dropping the sole released frame (`:376-379`).
   - Windows: the first `AcquireSurface` of a (revision, frame) pair (`:203-207`).
   - Dart doc comment: "Acknowledges consumption by the native texture, never queue acceptance"
     (`desktop_video.dart:6`).
   - Input authorisation depends on this through Dart `fresh` and `controlling`
     (`desktop_store.dart:125-140`) and the host's 2 s `recent` check (`worker.rs:402-414`). This
     must survive the overhaul.
2. **Revision fencing.** Every packet carries the revision current at enqueue. Outputs and events
   with a stale revision are dropped without rendering or emitting (Android `:189,333,396,448,480`;
   Windows `Current()` `:188,476,490,591`). An event is emitted only if it is still current when it
   reaches the platform thread.
3. **Ordered, gap-free H.264.** After any discard (overflow, generation, reset, failure) the decoder
   accepts only a keyframe, and the Android configure also needs SPS and PPS in it. Decoded
   pictures may be skipped (they keep references); compressed AUs are never skipped inside a GOP
   (`VideoPresentationQueue.kt:5,39-41`).
4. **Bounded everything.** Queue 4, pending 16, in-codec 4, surface 1, events coalesced to 2 per
   texture, textures 2. Producer credit is returned exactly once per submission
   (`Submission.completed` CAS `:66-71`; Windows replies are moved exactly once).
5. **No playback pacing.** A decoded picture is eligible at once and the newest wins
   (`VideoPresentationQueue.kt:21-26`). The test locks this in.
6. **Decoded visible rect equals the authorised frame (Windows).** Only an integral display
   aperture may crop macroblock padding (`desktop_video.cpp:343-369`). This keeps input
   coordinates consistent with what was shown.
7. **Never call blocking FFI on the UI isolate.** `pb_mapper_desktop_wait_frame` is documented as
   "never call this on the UI thread" (`desktop.rs:1242-1243`).
8. **Zoom, pan, rotation, keyboard and resize never touch native video.** The texture size comes
   from the encoded size only. The Activity survives configuration changes (`AndroidManifest.xml:28`),
   and Impeller stays off (`:19-21`).

## 6. Tests covering this subsystem

- `ui/android/app/src/test/kotlin/com/pb_mapper/ui/VideoPresentationQueueTest.kt` (JVM, JUnit 4):
  - `decodedPicturesAreImmediatelyEligibleAndBurstsDoNotReplayOlderPictures` (`:8-21`)
  - `pausedSurfaceDiscardsOnlySupersededDecodedPicturesAndRetainsNewestUntilConsumed` (`:24-45`)
  - `resetRetiresOldRevisionEvenWithNoSurfaceCredit` (`:48-62`)

  **No CI job runs Gradle unit tests.** `.github/workflows/syntax-check.yml` runs only
  `flutter analyze` and `flutter test`, and `release-ui.yml` only builds APKs.
- `DesktopVideo.kt` itself (MediaCodec, receipts, stall, reset, dispose): **no automated test.**
- `ui/windows/runner/desktop_video.cpp`: **no automated test.**
- `ui/test/desktop_lifecycle_test.dart` mocks the channel `pb_mapper/desktop_video`
  (`setMockMethodCallHandler`) and injects native `event`s through `ui.channelBuffers.push`
  (`:76-94`). Its cases:
  - the final presented frame is ACKed via the trailing timer, and teardown cancels it (`:98-143`);
  - worker cancellation releases the native borrow (`:144-197`; not channel-specific);
  - disconnect while busy disposes a late texture (`:199-241`);
  - native consumption must be current and recent (< 2 s) before input is live, status-only frames
    do not reset or refresh, and generation changes (`:243-334`).

  Three of these are `skip: !DesktopVideoTexture.supported`. CI runs `flutter test` on
  `ubuntu-latest`, so **they are skipped in CI** and run only on Windows hosts.
- Rust: `desktop.rs` test `binary_frame_is_owned_until_explicit_release_after_disconnect`
  (`:1725-1812`) covers wait_frame buffer ownership and its format. `media.rs` tests cover queue
  overflow, IDR waits and status coalescing (`:132-196`).
- Manual: `ui/tool/desktop_viewer_acceptance.dart` with `scripts/test/desktop-ui-acceptance.md`
  (native stats polling, Android VM numbers at lines ~104-107).

## 7. Impact on the planned overhaul

### P0-1: send credit on receipt; presentation ACKs remain only for input

- Native code is unaffected **as long as presentation receipts keep exactly today's meaning** (5.1).
  They stop driving host credit but still authorise input. Do not move the receipt to decode time
  or to `submit` acceptance.
- The local overload policy ("keep only the newest decodable frame; when the backlog exceeds a
  threshold ask for an IDR or lower fps") already exists in two places. Android and Windows drop
  everything when `queue >= 4` and emit needKeyframe; Rust `media::Queue` drops at 8 frames / 8 MiB
  and requests an IDR. In the new design that policy must sit in **one** place:
  - Today Dart's stop-and-wait makes the native queue bound unreachable (3.2), so the Rust queue
    is the real one.
  - With a direct handoff (P1-1), keep the native queue small (2-3) and let the native side stop
    pulling when it is full. The Rust queue then overflows → drop + IDR (`media.rs:47-54`).
  - The decode-or-present fps the Rust side needs, to cap host fps at 90 % of it, should come from
    native receipts, which Rust can count directly in the direct design.
- Risk: once credit no longer waits for presentation, a backgrounded app keeps receiving frames.
  Android then hits "texture was not consumed" after 2 s → `fail` → keyframe requests, which can
  loop (`DesktopVideo.kt:525-527`). Pause decode-side recovery on lifecycle pause, or have Dart stop
  requesting keyframes while the viewer is not in the foreground. Do not message the host on
  zoom/pan (P1-4); lifecycle is a separate matter.

### P0-3: infinite GOP

- Decoders need IDRs only for:
  - start;
  - a new generation;
  - a size change (Android `:486`/`:369`; Windows `:477-478`);
  - recovery after native drops or failures (3.5).

  These all already send needKeyframe. With an infinite GOP, **every** recovery path must request
  an IDR because none will arrive otherwise; all three native recovery paths already do. The
  Dart 700 ms watchdog (`desktop_store.dart:418-425`) is the backstop and must remain.
- Every Android IDR must still carry SPS and PPS inline (`DesktopVideo.kt:570`). If the encoder
  change (GOP max) also changes how parameter sets are emitted (for example only on the first IDR),
  Android configure will fail. Keep SPS and PPS on **every** IDR.
- Android rebuilds the codec on every revision bump, including overflow (3.6). With fewer IDRs that
  is rarer, but a hardware `createDecoderByType` plus `configure` costs real time. Consider
  `flush()`+`start()` for same-size recovery only if callback-index safety (`:365-367`) can be
  preserved. Otherwise keep the rebuild and simply avoid overflow.

### P0-4: targets and quality presets

- `desktop_api.dart:428-447` is Dart-only. Native decoders accept up to 7680×4320 but get
  1920×1080 requests. Changing to 1080p / ~4 Mbps / ≤60 fps needs no native change.
- Optional decoder hints for phones, to measure on a device: `MediaFormat.KEY_PRIORITY = 0`
  (realtime) and `KEY_OPERATING_RATE` = target fps in `configure` (`DesktopVideo.kt:371-379`), plus
  raising the `HandlerThread` priority (`:127`, e.g. `Process.THREAD_PRIORITY_DISPLAY`). These
  address DVFS-induced "burst, stall". Neither is set today.

### P1-1: phone pipeline. This is the main impact.

**What is wrong today (verified).** Each frame needs these hops, strictly serial with one frame in
flight:

1. worker isolate blocking FFI (≤16 ms wait);
2. isolate → main isolate;
3. main isolate → Android main thread (channel);
4. → `pb-desktop-video` worker;
5. worker → Android main thread (`reply.success`);
6. → main isolate;
7. → credit to the worker isolate (`pb_mapper_service.dart:180-184`) → next wait.

Steps 3 and 5 sit on the Android main thread, which also dispatches touch, IME and accessibility.
There are about 5 byte copies per frame. The receipt path needs another 4 hops plus the 33 ms
coalescing and the frame-isolate mailbox.

**Fallback (smallest change).**

- Android: build the channel with a background queue:
  `MethodChannel(messenger, "pb_mapper/desktop_video", StandardMethodCodec.INSTANCE, messenger.makeBackgroundTaskQueue())`.
  Then `submit` and its handler run off the main thread. `result.success` is legal from any
  thread, so `Submission.complete` could reply directly instead of `main.post`. Keep
  `channel.invokeMethod("event")` on the main thread (already `main.post`).
- `viewers` (`DesktopVideo.kt:25`) is then touched by the task-queue thread and by
  `DesktopVideo.dispose()` on main (from `cleanUpFlutterEngine`). Make it a `ConcurrentHashMap` or
  route `dispose` through the same queue.
- Dart: allow 2-3 submits in flight. `_consumeFrames` must stop awaiting each `submit`, and
  `DesktopFrameStream` credit must allow more than 1. The native `queue >= 4` overflow branch then
  becomes live, and it is correct.
- Windows has **no background TaskQueue for platform channels**, so its handler stays on the Win32
  thread. Only the "2-3 in flight" part applies there.

**Direct Rust → Kotlin handoff (preferred): requirements and simplest robust design.**

1. **Is the jni crate available?** Yes, offline.
   - `Cargo.lock` already contains `jni 0.21.1` (via `netdev`) and `jni 0.22.4` (via
     `hickory-net`/`hickory-proto`/`hickory-resolver`), plus `jni-sys 0.3.1`/`0.4.1` and
     `ndk-context 0.1.1`. All are Android-only transitive deps, and all are in the local cargo
     registry.
   - Add `[target.'cfg(target_os = "android")'.dependencies] jni = "0.21"` (or `"0.22"`, or raw
     `jni-sys` for 4-5 functions) to `ui/native/pb_mapper_ffi/Cargo.toml`. Following the workspace
     convention, pin it in root `[workspace.dependencies]` and use `.workspace = true`. Choosing one
     of the locked versions keeps `--locked` CI builds working.
   - Precedent: RustDesk uses `jni = "0.21"` for Android only (`<rustdesk checkout>\Cargo.toml:198-200`).
     Its same `.so` serves both Dart FFI and `Java_ffi_FFI_*` JNI exports
     (`src/flutter_ffi.rs:3066-3097`).
2. **Loading and JavaVM.**
   - Kotlin must call `System.loadLibrary("pb_mapper_ffi")`, for example in an
     `object DesktopVideoNative { init { ... } }`. ART resolves `external` methods only against
     libraries loaded through `System.load*` for the app class loader. Dart's
     `DynamicLibrary.open` (`dlopen` from `libflutter.so`) does not register the library with ART.
   - Both loads resolve to the same soname in the app's linker namespace, so they share one
     instance and its statics. `System.loadLibrary` calls `JNI_OnLoad` (if exported) even if Dart
     `dlopen`ed the library first. **Verify this on device**, for example with a static counter
     visible from both sides.
   - A pull design needs **no JavaVM and no `JNI_OnLoad`**: every call is a down-call from an
     already-attached Java thread that supplies its own `JNIEnv`. Use the
     `Java_com_pb_1mapper_ui_DesktopVideoNative_<method>` name mangling (the package's `_` becomes
     `_1`), or `RegisterNatives` in `JNI_OnLoad`.
   - A JavaVM is needed only for a Rust→Java push. Obtain it in `JNI_OnLoad(vm, _)` and store it in
     a `OnceLock`, or from `env.get_java_vm()` in an init method. You must also cache a `GlobalRef`
     to the class and its method IDs from a Java thread, because `FindClass` on a Rust-attached
     thread uses the system class loader and cannot see app classes.
   - Do **not** call `ndk_context::initialize_android_context` as a side effect. `netdev` probes it
     (`try_android_context` in netdev's `os/android/api.rs`) and would change behaviour.
     `hickory-resolver`'s Android `system_conf` would panic without it, but the code avoids system
     DNS config (`pb-mapper-core/src/addr.rs:251` uses `ResolverConfig::from_parts`).
3. **Which thread pumps.** Use one Kotlin `Thread("pb-desktop-feed")` per `Viewer`:
   - It loops `while (!closed)`.
   - It waits on a Kotlin condition while `queue.size >= 2`.
   - It calls `DesktopVideoNative.next(sinkId, meta: LongArray, timeoutMs = 50)`, which **blocks
     inside Rust**. That should be a `parking_lot::Condvar` on the media queue, not
     `runtime.block_on`, so the sink needs no tokio runtime and can outlive `PbMapperHandle`.
   - When it gets a unit, it runs today's `submit` logic minus the `MethodChannel.Result`:
     generation/revision/awaitingKey handling, enqueue, `schedulePump(0)`.
   - The codec, pump, surface and receipt code stays as it is on the `pb-desktop-video` worker.
   - Rejected alternatives:
     - a Rust push via JNI upcall (JavaVM and class caching, Rust tokio threads attached to ART,
       network reads blocked by Java work);
     - pumping on the codec thread with an eventfd registered through
       `MessageQueue.addOnFileDescriptorEventListener` (zero extra thread but novel);
     - moving decode into Rust via NDK `AMediaCodec` with `ANativeWindow_fromSurface` (a large
       rewrite that loses the tested Kotlin pipeline).
4. **Ownership and lifetime.** The Rust SDK adds a `VideoSink` from the `Viewer` in
   `crates/pb-mapper-desktop`, holding:
   - `Arc<media::Queue>` with an added Condvar and a **video-only** pop. Statuses and cursor no
     longer share this queue; that is P1-3.
   - `presented: watch::Sender<(u64,u64)>` (tokio 1.49 `watch::Sender` is `Clone`).
   - `keyframe: watch::Sender<u64>`.
   - the cancel token, so `wait` returns Closed after `Viewer::stop`.

   The FFI keeps a registry `u64 id → Arc<VideoSink>`. Dart obtains the id after `connect`, for
   example with a new desktop op `"video_sink"`, and passes it in
   `create {sinkId}`. Kotlin calls `close(id)` on dispose. Use **no raw pointers across the
   channel**: a stale id is harmless, whereas a raw `PbMapperHandle` address is not, because Dart
   destroys the handle in `_destroyIfIdle`, `pb_mapper_service.dart:454-462`, which today the frame
   isolate pins via `_activeCalls`.
5. **Data.** `next` returns a `byte[]` (`env.byte_array_from_slice`, one copy from `Arc<[u8]>`) and
   fills `meta = [status, generation, frameId, width, height, flags(keyframe), receivedMonotonicNs]`.
   Rust should take `receivedMonotonicNs` from `clock_gettime(CLOCK_MONOTONIC)` at network receipt,
   which is the same clock as Kotlin's `System.nanoTime()`. `receive_to_present` then becomes true
   viewer-side latency. Possible later optimisation: write straight into the codec input
   `ByteBuffer` (zero Java copy).
6. **Receipts and keyframes bypass Dart.**
   - In `consumeSurface` (`DesktopVideo.kt:198-201`) call `DesktopVideoNative.presented(sinkId, gen, frameId)`.
     It is non-blocking, a watch `send_replace`.
   - In `fail` and awaiting-key drops call `DesktopVideoNative.keyframe(sinkId)`, rate-limited.
   - Keep the coalesced `event` to Dart for UI freshness and geometry.
   - Move the "received less than 2 s ago" check from Dart (`desktop_store.dart:582`) into the
     Rust sink: keep a small ring of `(generation, frame_id) → received Instant` and reject stale or
     unknown receipts. That puts one enforcement point in front of both platforms. The host-side
     check stays.
7. **Dart consequences.** These couple with the Dart-state and FFI mappers:
   - `_acceptFrame`'s H.264 branch, `submit`, the `reset` barrier, `_videoStatuses` and the
     presented half of `_flushFeedback` become obsolete for H.264.
   - `fresh` (`desktop_store.dart:125-135`) currently requires `frame.bytes.isNotEmpty` and Dart's
     `_frameAt`. It must be redefined from native-presented events plus Rust receipt time and status.
   - The geometry for input comes from the status of the presented generation. Confirm in the
     worker map that display geometry is constant per generation; `invalidate()` bumps the
     generation (`worker.rs:392-401`).
   - The JPEG fallback must remain on the existing Dart path. The frame isolate then carries only
     JPEG and status, or is replaced by the P1-3 "latest status" poll.
8. **Safety.** Wrap every `extern "system"` JNI export in `catch_unwind` and return error codes;
   `unwrap`/`expect` are denied. Rust tests can cover `VideoSink` wait/close/overflow/stale-receipt.
   Kotlin feeder backpressure can be JVM-tested if the native calls sit behind a small interface.
   The existing `VideoPresentationQueueTest` stays as is.

**Windows equivalent: yes, Rust can hand frames to `desktop_video.cpp` directly.**

- Export a C ABI from the same `pb_mapper_ffi.dll` that shares the `VideoSink` core with the
  Android shim:
  - `pb_mapper_desktop_video_next(sink:u64, timeout_ms:u32, out:*mut *mut Unit) -> i32`
    (1 = unit, 0 = timeout, -1 = closed or unknown). `Unit` is `{data,len,generation,frame_id,width,height,keyframe,received_ns}`;
    it can borrow the `Arc<[u8]>` with no copy until the MF memcpy.
  - `pb_mapper_desktop_video_free(Unit*)`
  - `pb_mapper_desktop_video_presented(sink, generation, frame_id)`
  - `pb_mapper_desktop_video_keyframe(sink)`
  - `pb_mapper_desktop_video_close(sink)`
- The runner does not link the DLL, which is optional at CMake time. At `create`, resolve the
  functions with `GetModuleHandleW(L"pb_mapper_ffi.dll")`, or for a pinned reference
  `LoadLibraryExW(L"pb_mapper_ffi.dll", nullptr, LOAD_LIBRARY_SEARCH_APPLICATION_DIR)`, plus
  `GetProcAddress`. Dart has already loaded the DLL by then (`connect` precedes `create`). Fail
  `create` closed if any symbol is missing, since the DLL and exe ship together.
- Thread: the existing per-viewer `Run()` replaces `wake_.wait` (`:464`) with
  `next(sink, 50ms)`. It keeps `Feed`→`Take` on the same thread (the MFT is synchronous) and moves
  the generation/awaiting-key logic from `Submit` into `Run`. `Close()` calls `close(sink)` to wake
  it, then joins. `queue_`, the held replies and `Events::replies` go away for H.264.
- Receipts: `AcquireSurface` (raster thread) calls `presented(...)`, a non-blocking watch send
  with no runtime, next to today's event (`:220`). `Fail` calls `keyframe(sink)`.
- Clock: Rust on Windows should stamp `received_ns` from `QueryPerformanceCounter`, which is what
  MSVC `steady_clock` uses, so C++ can compute receive→acquire latency.
- A **C callback push** (`set_callback(sink, fn, ctx)` invoked on the tokio receive task right after
  `incoming.push`) is also possible, but not recommended:
  - it runs C++ (mutex + vector copy) on the network read task;
  - unregistration must wait for in-flight callbacks, which risks lock-order bugs with
    `mutex_`/`Close()`;
  - pull mirrors the Android design and keeps a single Rust core.
- Windows-only perf items found while mapping, to measure first:
  - a new shared BGRA `CreateTexture2D` per decoded frame (`:433-441`); prefer a small pool that
    respects outstanding `SurfaceLease`s;
  - the legacy `GetSharedHandle` without a keyed mutex, with only `context->Flush()` as
    cross-device synchronisation (`:96-100,450`);
  - stats with no timings.

### P1-2: delay probe on the network task

No native-decoder change. The probe never touches the decoders (`viewer.rs:225-235`).

### P1-3: input and cursor independent of video

- Today status-only updates share the frame stream and the decoder path filters them out
  (`desktop_store.dart:474-489`, `media.rs:72-77`). A direct handoff **forces** the split: the
  native sink carries only H.264 AUs, and cursor and status go to Dart through a non-blocking
  "latest" read.
- Both natives already ignore everything except video. No native change beyond 7.P1-1.
- `presented` and `keyframe` leave the frame-isolate mailbox automatically once native code calls
  Rust directly.

### P1-4: zoom, pan and resize stay local (invariant to keep)

What rebuilds a decoder or texture today:

- the native texture: only Dart `create`/`dispose` (per session, `_clearVideo`);
- the Android codec: a revision change (generation, overflow, reset, fail) or a size change;
- the Windows decoder: a size change or a failure (flush on revision).

Nothing in gestures, rotation, keyboard or window size reaches native code. That holds because of
the encoded-size-only texture, `configChanges` and `adjustResize` (`AndroidManifest.xml:28,30`),
and the ignored Windows size callback (`desktop_video.cpp:119`). Keep `EnableImpeller=false`
(`:19-21`). Moving to Impeller would require `createSurfaceProducer`, whose per-frame consumption
receipt semantics differ, so the receipt design (5.1) would need to be redone. Treat any Flutter
upgrade as a re-verification point for this opt-out.

### Regression guards (brief)

- Bounded queues: keep the native bounds (4.4). In the direct design add a native-side "do not pull
  when full" rule and keep `media::Queue`'s 8 / 8 MiB as the only overflow-to-IDR point.
- Reconnect never replays input: native decoders carry no input. A new texture per session
  (`_clearVideo`) and revision fencing ensure no old picture is ACKed into a new session.
- A fresh presented frame is still required for input (5.1). If receipts go native → Rust directly,
  enforce the 2 s staleness in the Rust sink and keep the Dart `fresh` gate.

### Requirement 2 (UI/UX) touch points

- **Connection quality overlay** (display fps, picture latency, codec, resolution):
  - Display fps: Android/Windows `stats.presented` delta. Dart also has `presentFps` from events
    (`desktop_store.dart:55-59,589-595`).
  - Codec: "H.264" plus Android `decoder`, the real MediaCodec name. Windows has only a fixed
    string; consider reporting the MFT or adapter.
  - Picture latency: Android `timings.receive_to_present` today misses Rust and Dart hops (4.2). It
    becomes end-to-end on the viewer only with the Rust receipt timestamp (7.P1-1 item 5). Windows
    needs timings added to `Stats()`.
  - Polling `stats` at 1 Hz is cheap. On Android it allocates `Diagnostics` on the first call
    because `timings` defaults on.
- **Six-state overlays**: native recovery reasons (3.5) are surfaced today as raw `error` strings
  (`desktop_store.dart:568-571`). Map them to the "recovering video" state instead of an error,
  especially size-change and awaiting-key recoveries.
- **Mobile minimap**: render a second `Texture(textureId: same)`. Do **not** call `create` again
  (the limit is 2 per process, and the second slot is effectively needed for overlap during
  reconnect). Android consumes each image once per frame. On Windows `AcquireSurface` may run once
  per Texture per frame, and the receipt dedupe (`desktop_video.cpp:203-207`) already handles that.
- **Last-picture thumbnail** (mobile; local only, deleted with the computer): neither native side
  has a readback path today.
  - Windows: add a channel method that copies `latest_` to a staging texture (optionally
    downscaled with the existing video processor), `Map`s it and returns BGRA bytes.
  - Android: first try a Dart `RenderRepaintBoundary.toImage` over the `Texture`, and verify on a
    device that external textures are captured with the GL renderer. The native alternatives (an
    `ImageReader` output instead of a SurfaceTexture, or GL readback) are invasive.
  - Never retain frames beyond this explicit thumbnail.
- **Quality preference** (Quality/Balanced/Smooth): this changes only bitrate and fps. A
  user-chosen resolution change needs an IDR at the new size; both natives rebuild the decoder on a
  size change.
- **Camera QR scan** needs `android.permission.CAMERA` in `AndroidManifest.xml` plus a scanning
  dependency. `build.gradle.kts` has none today, and minSdk is 24.

### Requirements 3 and 4 (icons, caption buttons)

No native-decoder impact. `FlutterWindow::MessageHandler` intercepts only `kWake`
(`WM_APP+0x413`) before Flutter, so it does not interact with caption-button work.

### Sharp edges found while mapping

- `ptsUs` is sent but ignored by both natives (`desktop_video.dart:87`). Drop it from the contract
  or use it.
- The Android overflow branch is unreachable today (3.2), so do not rely on it until Dart allows
  more than one submit in flight.
- The Android codec is fully rebuilt on every revision bump (3.6).
- Benign recoveries become user-visible `error` text (3.5).
- In the background, the Android 2 s consumption stall triggers repeated recovery (P0-1 risk above).
- On Windows `AcquireSurface` returns `nullptr` after `Reset` or `Fail` until a new frame decodes,
  so the picture blanks. Android keeps the last SurfaceTexture image.
- No native code or Kotlin unit test runs in CI, and the three native-path Dart lifecycle tests are
  skipped on the Linux CI runner (section 6).
