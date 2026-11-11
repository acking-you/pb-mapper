# pb-mapper-desktop's open items, measured, 2026-11-11

The performance entries for pb-mapper-desktop in
[the refactor's open items](../../refactor-2026-11-open-items.md): the
data-path allocations nobody had measured, the helper pipe's 1 ms drain
poll, and whether any other entry for the crate would make something
faster or lighter. Each change below was measured before and after on its
own path and kept only if it was better there; what was not changed says
why.

Only measured figures are below. The raw runs are in the
`open-items-desktop-*.txt` files next to this one (each run's result lines
in run order).

## Machine

The machine of [this pass's main record](README.md#machine), with:

| | |
|---|---|
| GPU | NVIDIA GeForce RTX 5070 Ti (driver 32.0.16.1088), which drives the display; the CPU's AMD Radeon graphics beside it |
| Display | 2560 × 1440 at 165 Hz |
| Encoder | NVIDIA H.264 Encoder MFT, the one `video_bench` picks |

Another agent compiled other crates on the machine meanwhile. Every run
waited while any `rustc`, `cargo` or `link` process existed, and the two
builds of a comparison alternated, so what load remained fell on both.

## Method

As in the main record: two release builds, the tree with and without the
change, alternating A then B on even rounds and B then A on odd ones;
tables give the median of each side with the smallest and largest run in
brackets, the change of the median, and a two-sided Mann-Whitney U
p-value.

The measures are those load moves least:

- `host_send_bench` counts every heap allocation and reallocation in the
  process, and now also the bytes each asks for (a reallocation counts as
  one allocation of its new size), per video unit. It runs a relay, a host
  whose "encoder" returns one 4 KiB H.264 unit per capture and a viewer
  that presents every unit, in one process at 60 fps: 5 s of video, then
  5 s of video while the viewer moves the pointer every 4 ms under control.
  The counts barely vary between runs.
- `video_bench` drives this machine's hardware encoder on its display at
  60 fps for 10 s, with its pattern window animating (`video_bench 10 60
  --animate`), and now prints a `case=encoder` line: what the encoder's own
  `submit` and `drain` calls allocate per unit, and the time and the cycles
  their thread ran in them (`QueryThreadCycleTime`, which counts only while
  the thread runs, so a preemption does not count). It changes nothing on
  the machine: no display mode, no input.

Where the allocations were came from a copy of `host_send_bench` (not
kept) that captured a backtrace on every allocation during 3 s of video,
then 3 s of video and pointer, at 634f158e; its listing, trimmed to four
frames a site, is `open-items-desktop-alloc-sites.txt` (the bench's own
types show there as `alloc_sites_tmp::`). Per video unit, video only:

| where | allocations per unit | what |
|---|---|---|
| thread C | 3.26 | a clone of the desktop context, which clones the display's name, for each capture, drain, pointer sample, input and lease renewal |
| B-send, `write_message` | 3.16 | the header's `to_vec` (1.08), its growth past 128 bytes for `Video` (1.00), and the `BufWriter` that copied header and payload together (1.08) |
| B-recv, `read_header` | 2.46 | the header record's `Vec` (1.23), serde's buffering of the tagged `ViewerMessage` (1.23) |
| viewer writer, `write_message` | 2.46 | `to_vec` (1.23), `BufWriter` (1.23) |
| viewer reader, `read_header` | 4.21 | the header record's `Vec` (1.08), serde's buffering of the tagged `HostMessage` (1.08) and its growth (2.05) |
| viewer reader, `read_payload` | 1.00 | the unit's own buffer, which goes to the video sink |
| rustls | 6.62 | each record's ciphertext (2.31), each received record's plaintext (2.31), the deframer buffer growing for a record and shrinking after (2.00) |
| pb-mapper-protocol | 4.62 | each encrypted frame's buffer, at the relay's legs and at both tunnel ends |
| the bench's "encoder" | 1.17 | the unit (1.00), the display's name (0.17) |
| other | 0.31 | tracing and the like |
| total | 29.27 | |

## Kept

Each "before" build is the tree with the item's files as the commit before
it has them; the bench changes are in both builds.

### Desktop messages through kept buffers (e275e35e)

`write_message` serialized the header into a new `Vec`, then copied header
and payload into a `BufWriter` sized for both and flushed it: two to three
allocations a message and a copy of every video payload. Each writer (B-send
and the viewer's) now keeps one buffer for the header record and the
payload's length and writes it with the payload in one vectored write,
`write_all_buf` of the two slices. rustls cuts a vectored write into the
records one write of the same bytes makes (it fragments its input as one
sequence), so the bytes on the wire and the TLS records are unchanged: a new
test writes messages of no payload, 3000 bytes and 100,000 bytes through a
TLS stream both ways and compares the records (44; 3048; six of 16401 and
one of 1744 bytes). Writing header and payload as two writes fails it
([48, 3017] against [3048]). Each reader (B-recv and the viewer's) keeps the
buffer it reads header records into, at most 8 KiB. Errors and their order
are unchanged: the header is serialized and both sizes are checked before
anything is written. A writer keeps the buffer of its largest header so far;
a header is at most 8 KiB, and a larger one is refused once serialized, as
before.

`host_send_bench`, before = 634f158e, after = e275e35e, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| video units_per_s | 60.00 [60.00..60.00] | 60.00 [59.80..60.00] | 0.0% | 0.705 |
| video allocs_per_s | 1760.0 [1758.0..1765.0] | 1342.0 [1339.0..1343.0] | -23.8% | 0.000 |
| video allocs_per_unit | 29.34 [29.30..29.43] | 22.38 [22.36..22.39] | -23.7% | 0.000 |
| video alloc_bytes_per_unit | 45254.0 [45254.0..45263.0] | 40234.5 [40234.0..40367.0] | -11.1% | 0.000 |
| video+pointer units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video+pointer inputs_per_s | 249.9 [249.7..250.0] | 249.9 [249.7..250.0] | -0.0% | 0.385 |
| video+pointer allocs_per_s | 5665.5 [5659.0..5669.0] | 4347.0 [4341.0..4351.0] | -23.3% | 0.000 |
| video+pointer allocs_per_unit | 94.45 [94.33..94.50] | 72.47 [72.37..72.52] | -23.3% | 0.000 |
| video+pointer alloc_bytes_per_unit | 53946.0 [53940.0..53947.0] | 47271.5 [47267.0..47274.0] | -12.4% | 0.000 |

Verdict: kept. 7.0 allocations and 5.0 KB fewer per unit, about 420
allocations a second at 60 fps, and 3.6 fewer per input event with the
pointer moving. The counts include the one allocation the vectored write
costs per video message: rustls collects the slices into a `Vec` before it
encrypts them.

### Thread C lends its desktop context (da68576c)

Thread C cloned its `DesktopContext`, and with it the display's name, to
hand the backend a reference: in each capture or drain step, pointer sample,
input and lease renewal. It now lends `&self.context` beside the backend,
the same value. The display-mode paths still clone: they run per command,
and `read_modes` takes the engine mutably.

`host_send_bench`, before = e275e35e, after = da68576c, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| video units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video allocs_per_s | 1342.0 [1341.0..1343.0] | 1148.0 [1147.0..1150.0] | -14.5% | 0.000 |
| video allocs_per_unit | 22.37 [22.35..22.38] | 19.13 [19.13..19.17] | -14.5% | 0.000 |
| video alloc_bytes_per_unit | 40234.0 [40234.0..40248.0] | 40192.0 [40192.0..40196.0] | -0.1% | 0.000 |
| video+pointer units_per_s | 60.00 [60.00..60.20] | 60.00 [60.00..60.00] | 0.0% | 0.705 |
| video+pointer inputs_per_s | 249.9 [249.9..250.0] | 249.9 [249.8..250.0] | -0.0% | 0.186 |
| video+pointer allocs_per_s | 4347.0 [4342.0..4351.0] | 3654.5 [3652.0..3660.0] | -15.9% | 0.000 |
| video+pointer allocs_per_unit | 72.47 [72.24..72.54] | 60.92 [60.87..61.03] | -15.9% | 0.000 |
| video+pointer alloc_bytes_per_unit | 47272.0 [47218.0..47287.0] | 47123.5 [47102.0..47138.0] | -0.3% | 0.000 |

Verdict: kept. 3.2 allocations fewer per unit on thread C, about 190 a
second, and 2.0 fewer per input event. The bench's display name is 13
bytes; a real display's name is longer.

With both, `host_send_bench` counts 19.13 allocations per unit where it
counted 29.34, and 60.92 with the pointer moving where it counted 94.45.

### The encoder keeps its NAL unit ranges (dc4ba14a)

Every access unit out of the Windows encoder was parsed into a new `Vec` of
its NAL unit ranges, to find its keyframes and parameter sets. The codec
now keeps that `Vec` and refills it; `AccessUnit` borrows it. The ranges
found are the same (a test parses into a used buffer and into a fresh one),
and so are the units sent. The sequence header read on a stream change, and
on a keyframe without parameter sets, still parses into a `Vec` of its own.

`video_bench 10 60 --animate`, before = da68576c, after = dc4ba14a, both with
dc4ba14a's bench less its cycle counts (added after these runs), 10 rounds
each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| encoder units | 595.0 [592.0..599.0] | 597.0 [592.0..600.0] | 0.3% | 0.307 |
| encoder allocs_per_unit | 2.292 [2.225..2.449] | 1.339 [1.231..1.477] | -41.6% | 0.000 |
| encoder alloc_bytes_per_unit | 26996.0 [26885.0..27311.0] | 26961.0 [26897.0..27126.0] | -0.1% | 0.545 |
| encoder submits | 595.0 [592.0..599.0] | 597.0 [592.0..600.0] | 0.3% | 0.307 |
| encoder submit_p50_us | 399.4 [373.1..450.4] | 396.2 [378.9..545.1] | -0.8% | 0.910 |
| encoder submit_mean_us | 428.8 [400.2..482.3] | 422.1 [403.2..596.7] | -1.6% | 0.496 |
| encoder drain_p50_us | 26.90 [25.70..29.20] | 24.75 [23.70..36.90] | -8.0% | 0.028 |
| encoder drain_mean_us | 30.90 [28.80..32.20] | 28.25 [26.70..42.20] | -8.6% | 0.026 |

Verdict: kept. One allocation fewer per access unit. The allocation count
is process-wide, so the pattern window's thread adds to both sides (the
spread of each); what remains per unit is the unit's own copy out of the
encoder's sample, which goes to the network. The drain times (-8%, p =
0.03) are not claimed.

### The video converter keeps its views (de4234ec)

The D3D11 video processor that scales each capture into the encoder's NV12
sample created an input view of the capture texture and an output view of
the sample's texture for every frame. The encoder copies every capture into
the same texture, and the samples come from a pool of four to six, so the
converter now keeps the input view and one output view per sample texture
and subresource, each with its texture (so a texture seen again is the same
one, not a new one at a freed address), at most six. The blit is the same:
a clip dumped through the change (`--dump`, pattern frames only) decodes
with ffmpeg without an error, every frame in order.

`video_bench 10 60 --animate`, before = dc4ba14a, after = de4234ec, both with
d4302c0d's bench, 15 rounds each:

| metric | before (n=15) | after (n=15) | change | p |
|---|---|---|---|---|
| encoder units | 597.0 [592.0..600.0] | 597.0 [592.0..600.0] | 0.0% | 0.709 |
| encoder allocs_per_unit | 1.339 [1.172..1.478] | 1.299 [1.157..1.484] | -3.0% | 0.130 |
| encoder alloc_bytes_per_unit | 26686.0 [26488.0..26836.0] | 26646.0 [26510.0..26785.0] | -0.1% | 0.724 |
| encoder calls_us_per_unit | 436.5 [423.5..496.8] | 415.2 [406.1..439.1] | -4.9% | 0.000 |
| encoder calls_kcycles_per_unit | 1321.9 [1277.1..1485.0] | 1238.1 [1201.6..1297.6] | -6.3% | 0.000 |
| encoder submits | 597.0 [592.0..600.0] | 597.0 [592.0..600.0] | 0.0% | 0.709 |
| encoder submit_p50_us | 372.7 [365.4..417.9] | 351.4 [342.1..371.5] | -5.7% | 0.000 |
| encoder submit_mean_us | 397.4 [384.2..453.0] | 374.6 [364.8..397.1] | -5.7% | 0.000 |
| encoder drain_p50_us | 21.00 [20.50..23.60] | 21.80 [21.20..24.20] | 3.8% | 0.005 |
| encoder drain_mean_us | 24.70 [24.00..26.60] | 25.50 [25.00..27.00] | 3.2% | 0.002 |
| encoder submit_p50_kcycles | 1109.0 [1083.2..1209.2] | 1010.1 [987.7..1069.0] | -8.9% | 0.000 |
| encoder submit_mean_kcycles | 1167.9 [1125.2..1314.7] | 1076.7 [1046.0..1133.8] | -7.8% | 0.000 |
| encoder drain_p50_kcycles | 89.40 [86.90..100.2] | 92.90 [89.90..101.9] | 3.9% | 0.004 |
| encoder drain_mean_kcycles | 102.6 [99.70..111.3] | 106.6 [104.6..112.5] | 3.9% | 0.000 |

`calls_*_per_unit` count every encoder call, an empty drain or a submit
that took nothing included: the thread that runs them (thread C, or the
helper's capture thread) spends 84 thousand cycles and 21 µs less per
frame. Two earlier sessions of the same change, with benches that did not
yet count every call, agree on the submit (-4.4% wall, p = 0.07, in one;
-8.9% cycles, p < 0.001, in the other) and disagree on the drain, whose
code did not change (+3.8% at p = 0.31, and +22% at p < 0.001); in each, the
submit's saving was larger than the drain's change. In every session the
bench's `mean_encode_ms` (submit to output) moved by less than 1.5%, inside
each side's spread. They are in the raw file.

Verdict: kept: 6.3% fewer cycles per frame in the encoder's calls.

## Not taken

### serde's buffering of the tagged messages

`HostMessage` and `ViewerMessage` are internally tagged, so serde collects
every field of a message into a `Vec` of buffered values before it picks the
variant, and `Input` inside an input event again: 4.36 allocations per unit
(the viewer's 3.13, B-recv's 1.23) and about three per input event, untouched
by the changes above. The wire bytes need not change to remove them, but the
decoding would have to be written by hand for each hot message: peek the
tag, then decode that message's fields into a struct that rejects unknown
fields, accepting and refusing exactly what the derived decoder does, its
sequence form included. That is a decoder redesign that needs a
differential test against the derived one over valid and broken input; not
done in this pass.

### rustls's own allocations

6.62 per unit at 634f158e, inside rustls 0.23: a ciphertext buffer per record
written, a plaintext `Vec` per record read, and the deframer's buffer
growing for a record larger than it holds and shrinking after. With the
vectored write, a `Vec` of the slices per video message joins them. None is
this crate's to change.

### Samples per frame

On this machine the encoder provides its output samples (output stream
flags 0x103: whole samples, one sample per buffer, provides samples), so no
sample or buffer is created per output; the converter's input sample per
submit comes from the pool allocated with the encoder. An encoder that does
not provide samples gets an `MFCreateSample` and an aligned buffer per
output; there is none here to measure.

### The helper pipe's framing

`oop::ipc` frames every request and answer on the helper pipe as the wire
did: a `to_vec` per JSON record, a `Vec` per record read, serde's buffering
of the tagged requests. The out-of-process backend is crate-private, so no
example reaches it, and nothing here measured it. Not taken.

### The helper pipe's drain polls every 1 ms

The poll waits for the hardware encoder's output. A `Video` request that
only drains (`submit: false`) carries `wait_ms`, which thread C sets to its
time to the next tick, at most 4 ms. `oop::server::drain` then calls
`drain_video(expected, 0)`, returns a unit if there is one, returns empty
once the encoder holds no unit or the wait is over, and otherwise sleeps
1 ms and looks again; thread C asks again at once after a drain that waited
its whole time. So the helper's capture thread wakes every millisecond for as
long as the encoder holds a unit, and a unit that comes out during a sleep
waits for the sleep's end. The poll cannot be replaced by a wake in
`drain` alone: the Windows encoder offers none. `Codec::events` reads the
encoder's event queue with `MF_EVENT_FLAG_NO_WAIT`, and in process thread C
polls the same encoder every 2 ms (`DRAIN_POLL`).

What a wake takes: the encoder asks for its events with
`IMFMediaEventGenerator::BeginGetEvent` and an `IMFAsyncCallback` (a COM
object the crate implements) that counts `METransformNeedInput` and
`METransformHaveOutput` and signals; `drain_video(expected, wait)` waits on
that signal until the request's deadline, and `oop::server::drain` makes one
call. In process, the same signal makes the Windows source a push source
(`VideoSource::DRAIN = None`), so thread C stops polling too.

Not done: the change is in the Windows encoder, which runs only on hardware.
The out-of-process tests run over an in-memory duplex with synthetic
backends: they would exercise the new wait, never the encoder's callback,
and the elevated helper cannot be installed here. Testing it takes
`video_bench --backend` extended to time output-to-collect latency and
wake-ups in process on hardware, and the helper installed to time a unit
across the pipe.

### The crate's other entries

No other pb-mapper-desktop entry on the page would make anything faster or
lighter: the decode-time validation, `PrivilegedSupport.reason`, the
helper's error kinds, the restore on drop, the strict service files, the
unsynced trust revoke and the unlogged control-pipe refusals are about wire
shapes, failure handling and logs. The one that touches size is enigo 0.6.1
pulling in windows 0.61 beside the workspace's 0.62 (a second copy of the
crate to compile and link); it needs an enigo release or a fork in `deps/`,
outside this crate.

## Commands

```text
cargo build --release --locked -p pb-mapper-desktop --example host_send_bench
cargo build --release --locked -p pb-mapper-desktop --example video_bench
host_send_bench.exe
video_bench.exe 10 60 --animate
video_bench.exe 4 60 --dump <clip> --frames 120
```

## Files

- `open-items-desktop-alloc-sites.txt`: where the allocations were, at
  634f158e, video and video with the pointer.
- `open-items-desktop-wire-host_send_bench.txt`,
  `open-items-desktop-context-host_send_bench.txt`: the two
  `host_send_bench` comparisons.
- `open-items-desktop-nal-video_bench.txt`: the NAL ranges.
- `open-items-desktop-views-video_bench.txt`: the converter's views, three
  sessions (`views-video` timed submits and drains in wall time,
  `views2-video` also in cycles, `views3-video` also every call).
