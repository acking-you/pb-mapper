# Issue #50 mobile recovery repair (build 914)

2026-11-06, Windows 11 Enterprise 10.0.26300, Ryzen 9 9950X,
RTX 5070 Ti, Rust 1.99.1, Flutter 3.44.9. This records software-boundary
acceptance, not physical-phone FPS or actual Windows secure-desktop acceptance.

## Checks

- `cargo fmt --all`
- `cargo clippy -p pb-mapper-desktop -p pb-mapper-ffi --all-targets --all-features -- -D warnings`: passed.
- `cargo test -p pb-mapper-desktop --all-features`: 29 unit tests and one
  direct/relay TLS integration test passed; two long acceptance tests ignored
  by default. The integration test includes 1,000 release requests without
  terminating video. The worker test holds capture unavailable for 4.1 seconds,
  then returns three asynchronous empty encoder results before a new IDR; old
  ACKs cannot credit the new generation, and new ACK/input resumes.
- `cargo test -p pb-mapper-ffi --all-features --lib`: 36 passed.
- `flutter analyze --no-pub`: no issues.
- `flutter test --no-pub test/desktop_recovery_test.dart test/desktop_lifecycle_test.dart test/desktop_test.dart test/desktop_gesture_test.dart test/desktop_host_readiness_test.dart`: 17 passed.
- Android `:app:testReleaseUnitTest -x :app:compileFlutterBuildRelease`:
  three production presentation-queue tests passed; release Kotlin compiled.
  The command temporarily clears an unavailable local Gradle proxy, without
  changing the user's persistent proxy settings.

## Slow transport and renderer recovery

Explicit command:

```text
cargo test -p pb-mapper-desktop --all-features --lib slow_link::presentation_feedback_recovers_after_slow_tls_and_renderer -- --ignored --nocapture
```

Selected output from this run:

```text
presentation_feedback second=8 bitrate=1315875 video=960x540 source_fps=31.85 ack_ms=180.86 frame=169 dropped=1
presentation_feedback second=12 bitrate=256000 video=640x360 source_fps=8.20 ack_ms=496.65 frame=213 dropped=1
presentation_feedback second=20 bitrate=2795757 video=1280x720 source_fps=56.42 ack_ms=17.50 frame=611 dropped=1
presentation_feedback second=30 bitrate=12000000 video=1280x720 source_fps=81.79 ack_ms=13.85 frame=1380 dropped=1
presentation_feedback accepted minimum_bitrate=256000 recovered_bitrate=12000000 max_valid_ack_ms=767.06 no_ack_stop_ms=4208.28 tls_bytes=12029663 final_reason=Renderer stopped acknowledging presented frames; reconnect the viewer
test result: ok. 1 passed; 0 failed; finished in 34.25s
```

This uses synthetic source frames over real TLS with deliberately limited
transport and simulated renderer feedback. It proves bounded adaptation and
recovery, including termination when presentation acknowledgments stop. The UI
reconnection policy is covered separately by the Dart tests. These rates are
not native displayed FPS or a claim about the user's phone or public network.

The new Windows preparation flow requires an installed, running helper and
authorized protected app for full sharing. Actual UAC consent, credential
prompts, lock-screen password entry and independent-network phone acceptance
remain outstanding; no local policy, service privilege or installer permission
was bypassed in these checks.
