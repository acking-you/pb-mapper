# Viewer following its window's display: measurements

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1, with other builds running on the machine at the same time. Three
runs of

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 slow_link::window_display`

Raw output: [run 1](run-1.txt), [run 2](run-2.txt), [run 3](run-3.txt).

Each case runs in one process on loopback, through a real relay and TLS, with
the slow-link suite's synthetic encoder (1080p, 8 ms, a per-frame budget) and
a simulated decoder taking 1 ms a unit. The viewer connects asking for
120 fps as its fastest display's rate (`fps_override` off), runs 10 s to
settle and 10 s on that display, then its window "moves" to a 60 Hz display
(`Viewer::set_display_refresh(60.0)`) for 10 s and back to a 144 Hz one
(`set_display_refresh(144.0)`) for 10 s. The SDK call stands in for the app's
`display_refresh` op; no window moved and no display mode changed. There is
no before: without this change no call exists, and the host stays at what
the viewer connected with.

## Smooth (ceiling 120)

Values are the three runs in order. "First capture at the new pace" is from
the call to the first capture the host's encoder was asked to pace at the
new rate, read from the synthetic encoder's own record of each pace change.
The rates are the means of the host's once-a-second stats over the phase,
skipping its first second; the received fps is the viewer's own, sampled
once a second over the whole phase, its first second included.

| Phase | Measure | Values |
| --- | --- | --- |
| On the 120 Hz display | host pace (fps) | 120.0, 120.0, 120.0 |
| | host cap | none in every second |
| | received (fps) | 119.9, 120.0, 119.9 |
| | sent (Mbit/s) | 12.01, 12.00, 11.99 |
| Moved to 60 Hz | first capture at pace 60 after (ms) | 64.6, 70.9, 73.3 |
| | host pace (fps) | 60.0, 60.0, 60.0 |
| | host cap | 60 in every second |
| | received (fps) | 66.2, 66.1, 66.2 |
| | sent (Mbit/s) | 6.00, 6.04, 6.00 |
| Moved back to 144 Hz | first capture at pace 120 after (ms) | 90.9, 107.4, 106.2 |
| | host pace (fps) | 120.0, 120.0, 120.0 |
| | host cap | none in every second |
| | received (fps) | 114.0, 114.0, 114.0 |
| | sent (Mbit/s) | 11.99, 11.94, 12.00 |

The pace changes within the 250 ms viewer tick and a loopback round trip,
each way, without a reconnect. The received means of the two moved phases
include a first sample that still reads the rate before the move: the
viewer's received rate is a once-a-second figure, and 66.1 to 66.2 over ten
samples is one at about 120 and nine at 60, 114.0 one at about 60 and nine at
120. Bits per frame
stayed at 100 000 throughout: on an unlimited link they are already the
preset's for this picture, so halving the frame rate halved the bitrate sent
rather than raising each frame's bits. On a link that limits the bitrate the
frames fewer would leave more bits for each, which these runs do not show.

## Balanced (ceiling 60)

| Phase | Measure | Values |
| --- | --- | --- |
| Every phase | host pace (fps) | 60.0 in each phase of each run |
| | received (fps) | 59.9 to 60.1 |
| | sent (Mbit/s) | 7.99 to 8.02 |
| | bits per frame | 133 333 |
| Moved to 60 Hz | host cap | 60 in every second |
| Moved back to 144 Hz | host cap | none in every second |

Balanced sends as it did: the display's 60 caps nothing its ceiling of 60
would send, and the host holds the limit only so that a switch to Smooth on
that display stays at 60.

## Not measured here

- The app end to end: the macOS viewer following a display switched from
  144 Hz to 60 Hz and back in System Settings, a window moved between a
  ProMotion panel and a 60 Hz external display. Switching a display's mode
  changes this computer's settings, which these runs did not do.
- A Windows viewer on a 59.94 or 119.88 Hz mode against a 60 or 120 Hz host
  on Balanced and Smooth: the quality panel's maximum frame rate and pace
  (60 against the 59 asked before), capture and received fps, and
  `desktop_capture_pacing`. The runner's `refresh_hz` and `display_changed`
  compile only in CI.
- The Linux runner's `refresh_hz` and `display_changed`, which compile only
  in CI: a window moved to another monitor under X11 and under Wayland
  (where GTK 3 reports no move between outputs of one scale), and a
  monitor's mode switched.
- A consumer that fell behind before its window moved: its backlog cap,
  held beneath the slower display, is what the host gets back on the faster
  one. The cap's unit tests cover that rule; these runs, whose consumer
  always kept up, have no backlog cap.
