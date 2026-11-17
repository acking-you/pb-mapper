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
