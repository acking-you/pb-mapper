# Remote desktop on an M3 Pro, 2026-11-17

PR #55 at `4e45282` and its macOS window lifecycle changes ran on a 12-core Apple M3 Pro, macOS 27.0.1 (26A434), Xcode 27.0, Flutter 3.44.9 and Rust 1.99.1. The app was an arm64 Release build of 0.11.0+928. The built-in Retina display reported 3456×2234 and the viewer reported 120 Hz. The external display was 2560×1440 at 60 Hz.

The SDK host and Flutter viewer ran on this one Mac over a TCP relay on loopback. The host was view-only. ScreenCaptureKit captured the external display with a moving 960×540 Core Animation window. The Balanced viewer requested 1920×1080 at up to 60 FPS. Separately, the replay host supplied a 1920×1080 H.264 moving test clip. The original acceptance entry crashed in the window plugin before connecting. Initializing the window as the product does allowed the viewer to run.

| Source | Duration | Presented FPS, median | Picture latency, median | Reconnects |
| --- | ---: | ---: | ---: | ---: |
| H.264 replay | 25 s | 38 | 14 ms | 0 |
| External display, H.265 | 30 s | 56 | 22 ms | 0 |
| External display, H.265, repeat | 30 s | 56 | 21 ms | 0 |
| External display, H.265, hide and restore | 36 s | 57 | 23 ms | 0 |

The acceptance tool discards its five-second warm-up and summarizes live samples. Picture latency is the SDK estimate. The replay source delivered a median 38.7 FPS. In the last run the viewer paused for four seconds and a fresh picture returned 51.6 ms after resuming. The 4.1 s presentation gap includes the deliberate pause. VideoToolbox reported hardware decoding for both codecs. Each run wrote a thumbnail when disconnecting.

A separate Release probe held the Remote page's host demand with no share active. It hid the app window with the product's window manager and counted calls through the real Desktop API during eight seconds of Flutter's hidden lifecycle state. Host status reads fell from eight to zero. Restoring the window resumed polling and returned both displays. The native capture probe also captured ten JPEGs from each display with the existing Screen Recording grant.

The counters and lifecycle events are in [measurements.json](measurements.json). To repeat the window cycle, use the [macOS viewer acceptance entry](../../../scripts/test/desktop-ui-acceptance.md#macos-viewer). Input, first-time system permission dialogs, physical monitor removal, logging out, and performance across two computers or a WAN need separate runs.

## Rust host send path

On the same M3 Pro and macOS 27.0.1, Rust 1.99.1 built the arm64 Release `host_send_bench` example at `520ced0` and with the capture-consent and recovery changes in PR #55. Each binary ran three times, in the order before, after, after, before, before, after. Each run measured five seconds of video, then five seconds of video with pointer events. The synthetic host supplies 4 KiB H.264 units. The relay, host and viewer run in one process, whose allocator supplies the counts below.

| Case | Build | Units/s | Inputs/s | Allocations/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: | ---: |
| Video | Before | 60.0 | 0.0 | 19.07 | 40,213 |
| Video | After | 60.0 | 0.0 | 19.07 | 40,213 |
| Video with pointer | Before | 60.0 | 249.8 | 58.99 | 47,101 |
| Video with pointer | After | 60.0 | 249.9 | 58.97 | 47,100 |

The table shows the median of each metric. The benchmark measures the sender with a synthetic encoder. The [per-run output](host-send-rust-review.txt) includes all six runs.

## November 18 data-path follow-up

The comparison used `b0e77c6` and the data-path implementation in `6a9848f`, built before its final borrow spelling, test and documentation edits. Both arms ran arm64 Release on the 12-core M3 Pro (36 GiB, AC power), macOS 27.0.1 (26A434), Rust 1.99.1, Flutter 3.44.9 and Xcode 27.0.

`host_send_bench` ran before, after, after, before with logging off. Each run measured five seconds of video followed by five seconds of video and synthetic pointer events. The relay, host and viewer share the process-wide allocator counter. The H.264 encoder produces fixed 4 KiB units.

| Case | Build | Units/s | Inputs/s | Allocations/unit | Allocated bytes/unit |
| --- | --- | ---: | ---: | ---: | ---: |
| Video | Before | 59.8 | 0.0 | 19.00 | 40,203 |
| Video | After | 60.0 | 0.0 | 19.01 | 40,206 |
| Video with pointer | Before | 59.8 | 246.6 | 58.59 | 47,028 |
| Video with pointer | After | 60.0 | 250.0 | 59.17 | 47,105 |

The table shows per-metric medians. Video-only allocations stayed within 18.96–19.03 per unit across both builds. The first baseline processed 243.3 pointer events/s, and the other runs processed 249.9–250. The [per-run output](host-send-data-path.txt) includes all four runs.

The view-only Flutter viewer ran over TCP relay on loopback. It requested Balanced 1920×1080 at up to 60 FPS and reported a 120 Hz viewer display. The replay host played the same 120-frame H.264 `testsrc2` clip at a requested 60 FPS with one IDR per 60 frames. For the native runs, ScreenCaptureKit captured a Core Animation pattern covering the 2560×1440, 60 Hz external display. The acceptance runner put its own window on the built-in display. Each run reports median FPS and SDK-estimated picture latency over live one-second samples after a five-second warm-up. Process CPU is user plus system CPU time divided by wall time over 18 seconds inside each 30-second run, starting seven seconds after the viewer launched. One fully used logical core is 100%. Replay host CPU includes the relay.

| Source | Build | Live samples | Presented FPS | Picture latency | Worst sampled interval p95 | Host CPU | Viewer CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| H.264 replay | Before | 26 | 38 | 14 ms | 34 ms | 2.90% | 10.41% |
| H.264 replay | After | 26 | 60 | 14 ms | 34 ms | 2.75% | 9.56% |
| H.264 external display | Before | 26 | 56 | 18 ms | 34 ms | 9.44% | 10.03% |
| H.264 external display | After | 26 | 60 | 17 ms | 34 ms | 11.58% | 13.76% |
| H.265 external display | Before | 26 | 57 | 25 ms | 34 ms | — | — |
| H.265 external display | After | 26 | 60 | 21 ms | 25 ms | — | — |
| H.265 external display, repeat | After | 26 | 60 | 20 ms | 34 ms | 12.10% | 15.57% |

The runs used VideoToolbox hardware decoding, completed without reconnecting and wrote a thumbnail at disconnect. The replay throughput includes the replay host's pacing fix. In the H.264 external-display pair, process CPU increased alongside presented FPS. In the H.265 pair, median reported encoder dwell went from 10.5 to 7.5 ms. The process CPU figures apply to this Mac and these individual runs.

The H.265 comparison used two steady runs whose setup asked for a window cycle through a flag the runner did not read. Both ran live for all 26 samples. Their CPU readings were discarded by the harness when it found no pause event. Other H.265 attempts were excluded after an unexpected window hide or resolution change. The configured hide-and-restore attempt was interrupted, so this follow-up provides no new lifecycle acceptance result. The [recorded measurements](data-path-measurements.json) include the per-run counters.
