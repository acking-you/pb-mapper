# Mac desktop check reuse: measurements

Thread C checks the desktop twice around every capture and twice around every
pointer read (`PlatformBackend::verify`). On a Mac each check reads the
login's session (`CGSessionCopyCurrentDictionary` and two lookups) and the
bound display's geometry (`CGDisplayBounds`, `CGDisplayCopyDisplayMode`) from
WindowServer. A Mac host now answers the checks around a capture or a pointer
read from a session check that passed, and a geometry read that matched,
within the last 16 ms (`platform/macos/reuse.rs`); the injection boundary, a
display mode change and the 100 ms context check still read WindowServer each
time (see
[Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding)).

## How it was measured

`session_check_cost`, an ignored test in `platform/macos/tests.rs`, opens the
main display and times the checks two ways:

- back to back: 10,000 rounds of a capture's two checks, 20,000 checks, in
  microseconds per check on the wall, of the test thread's CPU
  (`CLOCK_THREAD_CPUTIME_ID`) and of WindowServer's (`ps -o time`, to 10 ms);
- paced as thread C paces a 60 FPS session with the pointer sampled: two
  checks every 1/60 s and two every 16 ms, for 10 s, waiting on thread C's own
  waiter in between. It reports checks a second, the checks that took over
  10 µs (one that reads WindowServer), the WindowServer reads it counted, and
  milliseconds a second spent in checks, of the thread's CPU and of
  WindowServer's. An idle arm first waits the same way without checking, for
  the loop's own CPU and WindowServer's own.

`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture session_check`

**The screen of this Mac was locked** throughout, so its session check failed
(`the screen is locked`), and a failed check is never reused. The test
therefore made the same reads (`available()`, which reads the session
dictionary and both keys whatever it answers, and `geometry(id)` with the
match against the context's display) through the same `Reuse` policy, taking
the session's answer as a pass, and counted each read. It also timed
`MacDesktop`'s own recent checks while the lock failed them: each read
WindowServer, so a failure was not reused. On an unlocked Mac the test times
`MacDesktop`'s checks instead; that run is still to be made.

Before is the base, `perf2/wave1` (`bdbcd76`), with the same test where every
check makes both reads and nothing is reused, and `available` made
`pub(super)` for it ([its source](before-variant.rs.txt)); after is this
branch. The two release test binaries ran in turn, three runs each, a minute
or so apart: [before 1](before-1.txt), [after 1](after-1.txt),
[before 2](before-2.txt), [after 2](after-2.txt), [before 3](before-3.txt),
[after 3](after-3.txt).

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425),
Rust 1.99.1, main display 4096x2304 pixels, while other builds ran on the
machine (load average 2.0 to 6.9 at the runs' starts).

## Back to back

Microseconds per check, median of the three runs, range in brackets:

| Checks | Reads per check | Wall | Thread CPU | WindowServer CPU |
| --- | ---: | ---: | ---: | ---: |
| Before | 2 | 51.5 (50.5–54.5) | 18.1 (17.7–18.9) | 20.5 (20.0–22.0) |
| After, fresh (`recent = false`) | 2 | 51.9 (51.3–57.4) | 18.2 (18.0–20.1) | 20.5 (20.5–23.5) |
| After, recent (`recent = true`) | 0 | 0.039 (0.038–0.041) | 0.033 (0.033–0.035) | 0 |
| After, `MacDesktop` recent while locked | session only, every check | 37.2 (36.8–41.7) | 12.0 (11.9–13.4) | 11.5 (11.0–13.0) |

Back to back, the recent checks all fell within 16 ms of the fresh arm's last
read, so none of them read. The fresh path costs the same before and after.

## Paced at 60 FPS

Per second, median of the three runs, range in brackets. Every arm made
245.0 checks a second.

| Arm | Checks over 10 µs | Reads counted | In checks, wall (ms) | Thread CPU (ms) | WindowServer CPU (ms) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Idle, before | 0 | 0 | 0 | 1.17 (1.00–1.21) | 9 (8–34) |
| Idle, after | 0 | 0 | 0 | 1.16 (1.09–1.25) | 7 (5–8) |
| Before | 245.0 | 490.0 | 72.4 (68.0–81.9) | 25.9 (20.4–30.8) | 38 (36–70) |
| After, fresh | 245.0 | 490.0 | 74.8 (56.5–79.7) | 27.8 (13.1–29.4) | 36 (33–40) |
| After, recent | 61.3 (60.6–61.6) | 119.8 (119.6–119.9) | 24.4 (23.2–24.9) | 8.8 (8.6–9.9) | 17 (16–18) |

The idle arms include the pacing loop's own wakeups; WindowServer's idle CPU
varies with what else draws (34 ms/s in the first before run, against 5 to 9
in the others). With reuse, 61 of the 245 checks a second read WindowServer,
each making both reads; the thread spent 8.8 ms/s of CPU against 25.9 before
(the medians; 8.6–9.9 against 20.4–30.8), and WindowServer 17 ms/s against
36–38 with 5–9 idle.

Paced, a check that reads costs the thread several times its back-to-back
cost: 245 of them took 25.9 ms/s of CPU before, about 100 µs each against
18 µs back to back, as the thread wakes for each tick. The back-to-back figure
alone understates what reuse saves.

## Not measured here

These need a Mac whose owner is signed in with the screen unlocked, and the
live ones a viewer:

- `session_check_cost` there, which then times `MacDesktop`'s own checks;
- during `target/release/examples/deployed APP_CONFIG_PATH OUT/<arm>-<n>
  selftest 40 --tcp`, view-only and with the pointer moving, before and after:
  `sample <host pid> 10` for the share of `CGSessionCopyCurrentDictionary`,
  and `top -l 20 -stats pid,cpu,th` for thread C's CPU and WindowServer's.
