# Windows host kept awake: measurements

The Windows host now keeps the display and the system awake while a session
captures (`platform/windows/power.rs`, timing and the watchdog's loop in
`platform/awake.rs`; see
[Host: capture and encoding](../../remote-desktop.md#host-capture-and-encoding)).
Off Windows, no shipped build compiles any of it, so `host_send_bench` and the
other data-path examples on a Mac run the same code before and after; they
were not rerun.

## What a touch adds to a capture call

Every encoder submit (and open, and JPEG still) and every read of the pointer
calls `touch()`: a clock read, one sequentially consistent store and one load.
The figure below is a touch while the watchdog runs, which is every touch but
the first after an idle spell; that first one also wakes the watchdog
(`Thread::unpark`), which was not measured. The portable part,
`Touches::touch` with `Instant::now()`, measured beside a clock read alone,
20 million calls per figure, five rounds per run, three runs:

`cargo test --release -p pb-mapper-desktop --lib touch_cost -- --ignored --nocapture`

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1, while other builds ran on the machine. The first record is the
branch's first version, which reached the running state by calling
`Touches::run` by hand; the second is after review, which reaches it through
the watchdog's first look, as the Windows thread does. `Touches::touch` is the
same code in both.

| Record | | Clock read alone | Touch, clock included | Difference, per round |
| --- | --- | ---: | ---: | ---: |
| [First](touch_cost.txt), load average 16 to 17 | Median of the 15 rounds | 12.6 ns | 19.4 ns | 6.9 ns |
| | Range | 12.3–20.5 ns | 18.7–31.2 ns | 6.0–12.5 ns |
| [After review](touch_cost-after-review.txt), load average 12 to 14 | Median of the 15 rounds | 15.1 ns | 24.7 ns | 9.8 ns |
| | Range | 13.7–17.3 ns | 19.7–34.3 ns | 5.9–16.9 ns |

So a touch costs about 7 to 10 ns beyond the clock read on this loaded
machine, once per capture call or pointer read. On Windows the clock read is
`QueryPerformanceCounter`; that and the store were not measured there.

## Not measured here

The effect needs a Windows device, in the app and through the installed
desktop service:

- with the display-off timeout at 1 minute, a view-only viewer on a changing
  screen for 3 minutes: before, the picture freezes and `received_fps` falls
  to 0; after, it keeps updating;
- `powercfg /requests` lists the request under DISPLAY and SYSTEM while a
  session captures, also through a network stall of a few seconds, and none
  after a disconnect or 2–3 s into a pause;
- with the display off, connecting or resuming brings the first picture
  without local input.
