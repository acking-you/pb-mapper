# Refactor performance pass, 2026-11-11

Two questions, measured on one machine:

1. Did the refactor (3207797 → 03d1dd0f) slow a data path? Compared with
   the release-mode benches that already existed.
2. The performance findings earlier rounds left for measurement, and the
   pure run-time items in the audits' report-only lists: what each change
   does on its own path, before and after, and whether it stays.

Build times before and after are a separate record, [compile-time.md](compile-time.md).

Only measured figures are below. The raw runs are in the `*.txt` files next
to this one (each run's result lines in run order; the tracing log the
relay bench prints around them is left out).

Two later passes over [the open items](../../refactor-2026-11-open-items.md)
measured more changes, each in a record of its own beside this one:

- [open-items-auth.md](open-items-auth.md): the credential store's disk work
  taken off the runtime's threads, and the five-minute snapshot written only
  after a change.
- [open-items-relay.md](open-items-relay.md): first flights admitted through
  one replay-log writer that syncs per batch, subscribes waiting for a
  replacement woken instead of polling, and the forwarders' reads capped at
  1 MiB.
- [open-items-desktop.md](open-items-desktop.md): desktop messages written
  without copying their payload, thread C's context lent instead of cloned,
  and the Windows encoder's kept NAL ranges and converter views.

The commits named in this directory are commits of pull request #53's branch
(`acking-you/pb-mapper`), which master takes as one squashed commit: they
stay reachable through the pull request, not from master's history.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen 9 9950X, 16 cores / 32 threads, two CCDs |
| RAM | 61.6 GiB |
| OS | Windows 11 Enterprise 10.0.26300 |
| Power plan | Balanced (`381b4222-f694-41f0-9685-ff5bb260df2e`) |
| Rust | rustc 1.99.1 (58cf8f9c3 2026-10-13), LLVM 23.1.1, `x86_64-pc-windows-msvc` |
| Profile | the workspace's `release` profile, unchanged |

Nothing compiled during a timed run: each run waited while any `rustc`,
`cargo` or `link` process existed. The machine was otherwise not idle: a
headless QEMU VM kept about 1.85 cores busy throughout, and desktop
applications sat idle.

## Method

- A comparison runs two release binaries, built from the two trees or from
  one tree with and without the change, alternating: A then B on even
  rounds, B then A on odd ones, so drift falls on both alike. A "before"
  build is the same tree with the item's files as the commit before it has
  them.
- Each table gives, per metric, the median over all runs of each side with
  the smallest and largest run in brackets (`median [min..max]`), the change
  of the median, and a two-sided Mann-Whitney U p-value (normal
  approximation).
- `relay_transport_bench` is noisy here: in an A/A check (one binary on both
  sides) identical builds "differed" by up to 55% at p down to 0.004 (see
  "Noise checks"). Its comparisons therefore use 30 to 80 rounds per side,
  and a change in it is claimed only when it is large and p < 0.001.
- `framing_bench` moves by a few percent, and once by 14%, on rows whose
  code a change does not touch: code placement between builds. Its changed
  rows are judged against that.
- Benches written for this pass, each kept with the change it measures:
  `framing_bench` (pb-mapper-protocol), `auth_bench` (pb-mapper-auth) and
  `host_send_bench` (pb-mapper-desktop).

## Commands

From the repository root of each tree:

```text
cargo build --release --locked -p pb-mapper-cli --example relay_transport_bench
cargo build --release --locked -p pb-mapper-protocol --example data_codec_bench
cargo build --release --locked -p pb-mapper-galaxy --example galaxy_bench
cargo build --release --locked -p pb-mapper-protocol --example framing_bench
cargo build --release --locked -p pb-mapper-auth --example auth_bench
cargo build --release --locked -p pb-mapper-desktop --example host_send_bench
```

Each built `target/release/examples/<bench>.exe` was copied aside per tree
or per side, and the copies were run with no arguments, alternating as
above.

## Part 1: base 3207797 against the refactored 03d1dd0f

### relay_transport_bench

A 64 MiB echo, then 200 eight-byte round trips, through a relay in the same
process, for each carrier with and without the data codec. 80 rounds per
side, in three sessions of 10, 20 and 50 rounds.

| metric | base (n=80) | refactored (n=80) | change | p |
|---|---|---|---|---|
| Tcp codec=false MiB/s | 51.25 [10.20..71.90] | 48.05 [10.20..76.10] | -6.2% | 0.504 |
| Tcp codec=false p50 us | 171.7 [141.9..352.1] | 180.8 [140.8..305.3] | 5.3% | 0.434 |
| Tcp codec=false p95 us | 282.9 [216.5..476.6] | 289.5 [214.7..492.3] | 2.4% | 0.466 |
| Tcp codec=false p99 us | 435.2 [280.1..1874.7] | 416.2 [278.2..1348.4] | -4.4% | 0.624 |
| Tcp codec=true MiB/s | 45.95 [27.00..67.80] | 43.30 [9.000..76.80] | -5.8% | 0.313 |
| Tcp codec=true p50 us | 438.6 [355.5..748.8] | 451.1 [353.2..1032.6] | 2.9% | 0.196 |
| Tcp codec=true p95 us | 604.3 [446.8..1046.2] | 609.0 [440.9..1453.1] | 0.8% | 0.461 |
| Tcp codec=true p99 us | 814.5 [554.4..1575.7] | 873.4 [562.8..1954.4] | 7.2% | 0.327 |
| Quic codec=false MiB/s | 55.30 [41.50..76.30] | 55.80 [10.70..69.30] | 0.9% | 0.424 |
| Quic codec=false p50 us | 292.9 [220.4..449.0] | 297.4 [211.0..443.9] | 1.5% | 0.335 |
| Quic codec=false p95 us | 430.8 [275.5..648.9] | 432.3 [308.2..698.7] | 0.4% | 0.488 |
| Quic codec=false p99 us | 556.1 [421.4..21685.4] | 567.2 [404.4..16016.8] | 2.0% | 0.440 |
| Quic codec=true MiB/s | 47.50 [33.20..56.90] | 47.75 [36.00..59.40] | 0.5% | 0.797 |
| Quic codec=true p50 us | 308.6 [228.5..626.1] | 310.7 [234.6..517.1] | 0.7% | 0.985 |
| Quic codec=true p95 us | 458.3 [296.8..887.3] | 471.4 [283.3..796.7] | 2.9% | 0.560 |
| Quic codec=true p99 us | 604.3 [402.1..17208.1] | 585.6 [408.8..4312.8] | -3.1% | 0.471 |

Every p is above 0.19. The largest moves, the TCP throughputs (-6.2% and
-5.8%), sit inside a spread of 10 to 76 MiB/s on both sides.

### data_codec_bench

Codec setup and AES-GCM per message, legacy and v2. 6 rounds per side; each
run already prints the median of 9 samples.

| metric | base (n=6) | refactored (n=6) | change | p |
|---|---|---|---|---|
| 0B legacy ns | 193.4 [192.2..198.1] | 196.0 [192.1..209.9] | 1.4% | 0.522 |
| 0B v2 ns | 198.8 [195.6..203.3] | 201.1 [198.2..204.2] | 1.2% | 0.262 |
| 256B legacy ns | 127.8 [125.9..128.6] | 128.2 [127.3..129.8] | 0.3% | 0.337 |
| 256B v2 ns | 127.8 [125.8..128.5] | 128.2 [126.4..129.8] | 0.3% | 0.471 |
| 16384B legacy ns | 2473.9 [2443.9..2524.7] | 2460.7 [2449.0..2485.6] | -0.5% | 0.337 |
| 16384B v2 ns | 2470.4 [2444.2..2492.0] | 2454.1 [2443.1..2484.9] | -0.7% | 0.262 |

### galaxy_bench

One galaxy frame of a typical and of a busy scene. 20 rounds per side.

| metric | base (n=20) | refactored (n=20) | change | p |
|---|---|---|---|---|
| typical p50 us | 10.90 [10.80..11.00] | 10.80 [10.80..11.00] | -0.9% | 0.123 |
| typical p99 us | 14.45 [11.30..16.60] | 15.20 [12.30..18.00] | 5.2% | 0.194 |
| typical mean us | 10.90 [10.80..11.10] | 11.00 [10.80..11.40] | 0.9% | 0.304 |
| busy p50 us | 20.65 [20.50..20.80] | 20.70 [20.50..20.80] | 0.2% | 0.946 |
| busy p99 us | 26.25 [23.40..31.60] | 26.25 [23.30..32.60] | 0.0% | 0.705 |
| busy mean us | 20.90 [20.70..21.20] | 20.90 [20.70..22.00] | 0.0% | 0.766 |

Part 1's verdict: no data path the existing benches cover is slower in the
refactored tree, so no commit needed tracing.

### Stalls in relay_transport_bench

Now and then one 64 MiB echo takes 6-7 s instead of 1-1.5 s (it shows as
9-11 MiB/s), in any of the four cases; nothing is logged while it waits, and
the run then finishes normally. The cause is Windows' loopback TCP, not
pb-mapper (below). Once, on the after side of the one-write comparison
(round 18, `Tcp codec=false`, a path that change does not reach), the echo
did not finish within the bench's 60 s timeout and the run panicked; that
run has no figures. It did not recur in any run below; it may have been
several stalls in a row, which is not verified.

The first part-1 sessions showed stalls on the refactored side only (0 of 30
base runs, 3 of 30 refactored), so a third session ran base against
03d1dd0f for 50 more rounds. Runs with a stall, every session of this pass:

| session | side | build | runs | stalled | unfinished |
|---|---|---|---|---|---|
| part 1, sessions 1-2 | A | base 3207797 | 30 | 0 | 0 |
| | B | 03d1dd0f | 30 | 3 | 0 |
| part 1, session 3 | A | base 3207797 | 50 | 2 | 0 |
| | B | 03d1dd0f | 50 | 2 | 0 |
| u05-02 | before | 03d1dd0f | 30 | 1 | 0 |
| | after | 91c6d5d5 | 30 | 2 | 0 |
| one-write | before | 967847e1 | 60 | 2 | 0 |
| | after | 006152f8 | 60 | 5 | 1 |

Base stalls too (2 of 80 runs, against 5 of 80 for 03d1dd0f: Fisher exact
p = 0.44; 2 of 80 against 16 of 260 for every build after the refactor:
p = 0.26). A final comparison ran base 3207797 against the refactored tree
(5ab03df8) for 200 alternated rounds per side
(`stall-final-relay_transport_bench.txt`): 10 runs of each stalled. The
refactor did not introduce the stalls and does not change how often they
come.

#### What a stall is

At every five-second stall examined but one (below), one loopback
connection is stuck the same way, read with `SIO_TCP_INFO` and `FIONREAD`
on every socket:

- the sending end's TCP holds a send window under one loopback MSS (65495
  bytes): `SndWnd` 256 to 1280 bytes, nothing in flight. The program writing
  to it is blocked on a full send buffer, as it should be.
- the receiving end's TCP advertises its whole buffer (`RcvWnd` 131070 or
  262140) and holds nothing unread (`FIONREAD` 0). The program reading it
  is waiting for data, as it should be.

About five seconds later the sender's window jumps to the full window and
the bytes flow at once. No program holds them: the two ends of one TCP
connection disagree about its window. Whether the update that reopens it
is never sent, dropped or not applied would take a packet capture, which
needs administrator rights the account running these benches lacked.

This is what Microsoft's KB2020447 describes: over loopback on a
multiprocessor computer, a transfer of more than 64 KB can wait five seconds
for the silly-window-syndrome avoidance timer, and a receiver that
acknowledges within those five seconds does not end the wait. The article
offers no fix, only fewer occurrences with more receive buffer or smaller
transfers. Seen here on Windows 11 Enterprise 10.0.26300 (build
26300.9711). Any loopback hop of a real tunnel on Windows can meet it: a
local program to the `connect` end, or the `register` end to a backend on
the same computer. It is not specific to an echo: a one-way upload through
three pb-mapper forwarders stalled for 5.39 s.

#### How it was shown not to be pb-mapper's

A reproducer looped the bench's echo (`write_all` and `read_exact` of
64 MiB on one socket) through a chain of forwarders and, after a second
without progress, printed the state above for every socket. These runs went
three to seven at a time, which raised the rate; a stall counted is a pause
of 3 s or more:

| program | rounds | stalls |
|---|---|---|
| std only: blocking sockets, a thread per direction, 3 forwarders | 206 | 7 (4.6-5.0 s) |
| tokio only, 3 forwarders, tokio 1.49.0 / mio 1.1.1 | 181 | 15 (4.5-5.0 s) |
| tokio only, 3 forwarders, tokio 1.54.1 / mio 1.2.4 | 182 | 14 (4.3-5.0 s) |
| pb-mapper's forwarder, 3 hops, no relay | 283 | 14 |
| `tokio::io::copy_bidirectional`, 3 hops | 208 | 0 |
| relay, TCP carrier, codec on | 40 | 6 |
| Linux (WSL2, kernel 6.6.87.2): pb-mapper's forwarder, 3 hops | 277 | 0 |
| Linux (WSL2, kernel 6.6.87.2): relay, TCP carrier, codec off | 266 | 0 |

- The std-only and tokio-only programs contain no pb-mapper code and stall
  with the same TCP state. The std one has neither tokio nor mio, and at
  every stall the receiver had nothing to read and the sender was not
  writable, so no wakeup was lost either.
- Linux never stalled, in 543 rounds.
- The relay, QUIC and the codec play no part: chains without a relay stall,
  and so do all four bench cases.
- Forwarders that read and write in large chunks meet it more often than
  8 KiB copies (above, and a plain copy loop: 4 stalls in 221 rounds with
  an 8 MiB buffer, none in 170 with 8 KiB). They make a receive window
  fall under one MSS more often; they do not hold the bytes.
- Nothing on the data path waits five seconds: the forward loop's only
  timers are the tunnel idle timeout (1 h) and the half-close idle timeout
  (60 s), and QUIC's keep-alive is 15 s.

Under that load the reproducers also showed pauses, mostly of 1-2 s, in
which a runnable task or thread (pb-mapper's, `copy_bidirectional`'s and
the std threads alike) had data to move and did not run: an overloaded
CPU, not the stale window. The one five-second stall that did not show the
stale window, a relay pause of 5.37 s taken while the most reproducers ran,
was of this kind. The final comparison shows none: none of its echoes took
between 2.5 and 5.5 s.

#### Workarounds measured and not taken

| change | measured with | result |
|---|---|---|
| Forward reads capped at 64 KiB (`MAX_BUF_SIZE` in `pb-mapper-protocol/src/buffer.rs`) | `relay_transport_bench`, 57 alternated rounds per side | runs with a stall: 3 of 57 before, 3 of 57 after. QUIC throughput -16.9% without the codec (p = 2.3e-8) and -7.7% with it (p = 0.007); TCP unchanged (p > 0.05) |
| A 4 MiB `SO_RCVBUF` on every loopback data socket pb-mapper owns | `relay_transport_bench`, 33 alternated rounds per setting | runs with a stall: 2 of 33 without, 1 of 33 with; throughput not distinguishable |

A larger receive buffer makes the state rarer, as the KB says, but only
where pb-mapper owns the receiving socket, and in the bench the stuck
receiver was usually the bench's own. Neither removed the stall; the first
costs QUIC throughput, and the second is a Windows-only socket option that
costs memory per socket. Neither is applied.

#### Reading the tables

Echo times are bimodal. Of the final comparison's 1600 echoes, 1580 took
0.5-2.5 s, 19 took 5.5-7.0 s and one 11.49 s (two stalls in one echo); none
took between 2.5 and 5.5 s. A stall adds about five seconds to one echo
(4.2-5.4 s where the reproducers timed it) and nothing else. In this pass's
raw files an echo with one stall shows as 9.0-10.8 MiB/s, the one with two
as 5.6 MiB/s, and the slowest echo without a stall as 23.5 MiB/s.

So count a run with an echo over 5 s as stalled, and leave stalled runs out
of throughput medians or give the medians both ways. A stall barely moves a
median but sets the minimum, and the [min..max] spreads above that reach
down to 9-11 MiB/s are stalls. A stall count says nothing about the build.
The final comparison both ways (MiB/s, median [min..max]):

| case | side | stalled echoes | all 200 runs | the 190 runs without a stall |
|---|---|---|---|---|
| Tcp codec=false | base | 8 | 49.55 [5.6..77.2] | 50.00 [28.3..77.2] |
| | refactored | 4 | 50.90 [9.6..80.2] | 50.90 [30.0..80.2] |
| Tcp codec=true | base | 0 | 43.10 [27.3..66.5] | 43.40 [27.3..66.5] |
| | refactored | 4 | 46.75 [9.3..66.5] | 46.85 [28.5..66.5] |
| Quic codec=false | base | 2 | 63.50 [10.6..76.5] | 63.50 [44.4..76.5] |
| | refactored | 1 | 63.70 [10.8..74.9] | 63.75 [46.3..74.9] |
| Quic codec=true | base | 0 | 55.30 [38.5..73.3] | 55.30 [41.4..73.3] |
| | refactored | 1 | 56.55 [10.6..68.9] | 57.25 [40.7..68.9] |

## Part 2: the deferred findings and report-only items

### u05-02: the relay's forwarding body as one generic function (kept, 91c6d5d5)

`with_relay_stream!` pasted the subscriber-to-provider forwarding body once
per carrier pair, four times with `relay-quic`. It is now `relay_legs`,
generic over each leg's halves, so every pair stays statically dispatched;
the relay codecs are built once, before the dispatch, after the setup
responses as before. This is a duplication fix first; the measurement
checks that the extra async-fn layer costs nothing.

`relay_transport_bench`, before = 03d1dd0f, after = 91c6d5d5, 30 rounds
each:

| metric | before (n=30) | after (n=30) | change | p |
|---|---|---|---|---|
| Tcp codec=false MiB/s | 55.70 [30.60..72.60] | 54.25 [10.20..74.40] | -2.6% | 0.836 |
| Tcp codec=false p50 us | 169.7 [140.5..313.9] | 156.7 [139.6..225.6] | -7.7% | 0.010 |
| Tcp codec=false p95 us | 281.2 [215.9..536.5] | 260.9 [174.7..413.1] | -7.2% | 0.029 |
| Tcp codec=false p99 us | 437.1 [316.2..1216.6] | 387.5 [326.8..655.8] | -11.3% | 0.040 |
| Tcp codec=true MiB/s | 46.40 [26.70..65.60] | 49.05 [10.60..62.10] | 5.7% | 0.595 |
| Tcp codec=true p50 us | 424.6 [355.4..653.8] | 416.6 [368.8..811.1] | -1.9% | 0.641 |
| Tcp codec=true p95 us | 579.6 [428.5..998.1] | 564.0 [417.6..1155.5] | -2.7% | 0.478 |
| Tcp codec=true p99 us | 840.3 [533.2..1534.2] | 801.5 [510.6..1573.0] | -4.6% | 0.383 |
| Quic codec=false MiB/s | 57.80 [10.30..73.60] | 54.70 [41.30..70.00] | -5.4% | 0.853 |
| Quic codec=false p50 us | 298.7 [216.3..411.6] | 292.8 [262.0..466.1] | -2.0% | 0.947 |
| Quic codec=false p95 us | 434.1 [352.9..659.7] | 464.7 [362.4..646.9] | 7.1% | 0.329 |
| Quic codec=false p99 us | 568.8 [425.1..13414.0] | 607.4 [452.1..13852.2] | 6.8% | 0.399 |
| Quic codec=true MiB/s | 47.80 [32.10..52.60] | 45.35 [34.10..54.00] | -5.1% | 0.249 |
| Quic codec=true p50 us | 302.8 [275.7..484.9] | 309.0 [244.7..388.2] | 2.0% | 0.877 |
| Quic codec=true p95 us | 473.9 [418.8..744.6] | 459.6 [381.8..607.3] | -3.0% | 0.615 |
| Quic codec=true p99 us | 633.3 [483.5..9451.4] | 625.9 [447.7..1567.0] | -1.2% | 0.460 |

No metric is slower beyond the noise (the worst, QUIC p95 +7.1%, at
p = 0.33). The TCP latency gains (p 0.01 to 0.04) are of the size
the A/A check produced by chance and are not claimed. Verdict: kept, no
slower and simpler.

### u01-05: the process credential's checksum in one atomic (kept, 77c65aa6)

`valid_checksum` took two read locks (cloning the load error) and an atomic
load per frame keyed by the process credential. Readiness and the hash are
now one packed `AtomicU64`, stored under the credential's lock, and the
start's refusal is the credential slot's `Err` instead of a second lock.
Results are the same; the window in which a rotation could pair the new
readiness with the old hash is gone.

No relay or tunnel leg frames with the process credential (every one sets a
session key), so `relay_transport_bench` does not reach this path; it is
the SDK's `get_header_msg_reader` / `get_header_msg_writer` framing, which
`framing_bench`'s `process` rows drive.

`framing_bench`, before = 91c6d5d5, after = 77c65aa6, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| process-16B write_ns | 22.21 [21.58..22.48] | 8.725 [8.590..9.090] | -60.7% | 0.000 |
| process-16B read_ns | 28.21 [25.51..32.59] | 15.10 [14.86..15.48] | -46.5% | 0.000 |
| process-1024B write_ns | 26.01 [25.44..27.43] | 17.97 [17.40..19.82] | -30.9% | 0.000 |
| process-1024B read_ns | 33.50 [30.30..39.19] | 21.29 [20.43..22.33] | -36.5% | 0.000 |
| session-16B write_ns | 17.10 [17.05..17.39] | 17.27 [17.13..17.62] | 1.0% | 0.011 |
| session-16B read_ns | 26.34 [26.16..26.45] | 27.06 [26.65..27.52] | 2.8% | 0.000 |
| session-1024B write_ns | 27.57 [26.73..28.45] | 27.95 [27.31..28.56] | 1.4% | 0.199 |
| session-1024B read_ns | 33.21 [31.97..35.85] | 32.25 [31.49..33.65] | -2.9% | 0.049 |
| datagram-64B write_ns | 109.8 [108.9..111.2] | 109.7 [108.9..113.4] | -0.1% | 0.821 |
| datagram-64B read_ns | 113.6 [103.0..118.5] | 113.1 [110.6..119.8] | -0.5% | 0.521 |
| datagram-1200B write_ns | 223.9 [219.5..226.8] | 222.6 [219.8..229.3] | -0.6% | 0.406 |
| datagram-1200B read_ns | 191.0 [189.3..194.4] | 189.8 [186.8..195.5] | -0.6% | 0.406 |
| stream-1024B write_ns | 175.9 [174.1..177.7] | 176.1 [174.3..178.1] | 0.1% | 0.650 |
| stream-1024B read_ns | 143.8 [141.3..146.3] | 144.7 [141.5..145.8] | 0.6% | 0.880 |
| stream-8192B write_ns | 941.3 [920.5..975.7] | 946.6 [934.2..974.7] | 0.6% | 0.273 |
| stream-8192B read_ns | 758.7 [743.0..790.5] | 767.3 [755.9..778.1] | 1.1% | 0.082 |

Verdict: kept. The `process` rows are the changed path, 31-61% faster; the
other rows are controls, which moved by the code placement noise described
under "Method".

### Report-only (u03): a codec datagram copied twice (kept, 967847e1)

`CodecDatagramWriter::send` copied each datagram into a new `Vec` before
passing it to the codec writer, which copies it again to encrypt it in
place. The first copy is gone; bytes, order and errors are unchanged.

`framing_bench`, before = 77c65aa6, after = 967847e1, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| process-16B write_ns | 8.630 [8.530..8.890] | 8.390 [8.340..8.540] | -2.8% | 0.000 |
| process-16B read_ns | 14.59 [14.45..14.72] | 14.12 [14.01..14.40] | -3.2% | 0.000 |
| process-1024B write_ns | 20.00 [19.25..22.39] | 18.69 [17.34..21.92] | -6.5% | 0.054 |
| process-1024B read_ns | 22.17 [20.95..23.29] | 23.10 [21.84..24.32] | 4.2% | 0.023 |
| session-16B write_ns | 17.29 [17.24..17.46] | 17.16 [17.10..17.23] | -0.8% | 0.000 |
| session-16B read_ns | 26.06 [25.98..26.32] | 25.99 [25.93..26.28] | -0.3% | 0.049 |
| session-1024B write_ns | 29.54 [28.75..31.05] | 28.52 [27.82..29.88] | -3.5% | 0.004 |
| session-1024B read_ns | 32.66 [30.91..33.75] | 32.43 [31.48..33.55] | -0.7% | 0.597 |
| datagram-64B write_ns | 110.9 [108.8..268.6] | 91.87 [90.89..97.58] | -17.1% | 0.000 |
| datagram-64B read_ns | 112.8 [102.0..113.8] | 111.6 [102.2..113.0] | -1.1% | 0.131 |
| datagram-1200B write_ns | 223.0 [221.2..227.8] | 196.6 [194.6..203.6] | -11.8% | 0.000 |
| datagram-1200B read_ns | 189.9 [186.8..192.5] | 188.1 [186.2..191.9] | -1.0% | 0.151 |
| stream-1024B write_ns | 175.9 [174.3..177.4] | 175.3 [173.0..180.4] | -0.3% | 0.199 |
| stream-1024B read_ns | 143.6 [141.1..145.4] | 144.6 [143.1..145.8] | 0.7% | 0.151 |
| stream-8192B write_ns | 948.2 [936.6..970.9] | 939.2 [914.0..976.7] | -0.9% | 0.064 |
| stream-8192B read_ns | 753.0 [740.7..784.5] | 756.8 [735.9..778.3] | 0.5% | 0.705 |

Verdict: kept. The changed rows, the encrypted datagram writes, are 12-17%
faster.

### Report-only (u03): an encrypted frame in one write (kept, 006152f8)

`CodecMessageWriter::write_msg` wrote every encrypted frame as four writes:
checksum, length, ciphertext, tag. On a TCP leg with `TCP_NODELAY` each is
a send of its own. The frame is now built whole in the buffer it is
encrypted in (header placeholder, copy of the message, then the tag) and
written in one go. The bytes on the wire, the one allocation per frame and
the order of encryption and checksum are unchanged, and a write that fails
part-way still reports the field it failed in (a new test cuts a frame at
each field's boundary; the old writer passes it too). Every encrypted data
leg (the relay and both tunnel ends) and the legacy control session write
through it.

`relay_transport_bench`, before = 967847e1, after = 006152f8, two
sessions of 30 rounds (60 per side). Both builds also carried the then
uncommitted auth change below; only `framing.rs` differed.

| metric | before (n=60) | after (n=59) | change | p |
|---|---|---|---|---|
| Tcp codec=false MiB/s | 51.70 [9.100..68.30] | 54.10 [9.800..73.40] | 4.6% | 0.604 |
| Tcp codec=false p50 us | 167.7 [142.2..281.3] | 161.5 [138.9..362.8] | -3.7% | 0.386 |
| Tcp codec=false p95 us | 281.4 [218.2..407.7] | 272.9 [215.8..544.5] | -3.0% | 0.385 |
| Tcp codec=false p99 us | 423.3 [275.0..656.3] | 403.2 [299.9..1606.4] | -4.7% | 0.381 |
| Tcp codec=true MiB/s | 45.70 [9.800..60.90] | 46.40 [9.200..73.50] | 1.5% | 0.752 |
| Tcp codec=true p50 us | 430.6 [345.3..922.6] | 197.0 [170.0..346.5] | -54.2% | 0.000 |
| Tcp codec=true p95 us | 569.3 [429.6..1147.2] | 292.9 [208.5..484.1] | -48.5% | 0.000 |
| Tcp codec=true p99 us | 792.3 [590.5..1434.2] | 443.5 [330.2..911.0] | -44.0% | 0.000 |
| Quic codec=false MiB/s | 55.80 [40.60..70.40] | 58.20 [38.70..73.60] | 4.3% | 0.505 |
| Quic codec=false p50 us | 288.1 [217.8..458.5] | 293.0 [213.4..473.6] | 1.7% | 0.748 |
| Quic codec=false p95 us | 445.4 [295.8..732.0] | 443.2 [318.1..701.0] | -0.5% | 0.736 |
| Quic codec=false p99 us | 574.6 [435.1..5087.9] | 593.2 [384.2..8112.9] | 3.2% | 0.930 |
| Quic codec=true MiB/s | 46.00 [34.70..58.60] | 48.30 [34.80..59.60] | 5.0% | 0.029 |
| Quic codec=true p50 us | 307.4 [229.3..481.6] | 304.9 [232.7..538.9] | -0.8% | 0.819 |
| Quic codec=true p95 us | 465.2 [300.6..708.3] | 453.9 [331.7..760.4] | -2.4% | 0.813 |
| Quic codec=true p99 us | 599.6 [459.9..10267.7] | 597.6 [456.3..18001.6] | -0.3% | 0.896 |

The encrypted TCP round trip halves: p50 -54%, p95 -49%, p99 -44%, each at
p < 0.001. Nothing is slower beyond the noise. The two sessions disagreed on encrypted TCP
throughput (-11% at p = 0.05 in the first, +19% at p = 0.03 in the second);
pooled it is +1.5% at p = 0.75. Encrypted QUIC throughput, +5.0% at
p = 0.03, is not claimed. One after-side run never finished its
`Tcp codec=false` echo, a path this change does not reach; see "Stalls".

In memory, with no sends to save, `framing_bench` shows that building the
frame costs what the four writes did (the changed rows are the `stream` and
`datagram` writes):

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| process-16B write_ns | 8.450 [8.380..8.730] | 8.755 [8.480..8.910] | 3.6% | 0.004 |
| process-16B read_ns | 14.84 [14.59..15.93] | 14.05 [13.85..14.53] | -5.3% | 0.000 |
| process-1024B write_ns | 21.73 [20.00..24.62] | 21.24 [19.97..24.29] | -2.3% | 0.571 |
| process-1024B read_ns | 23.10 [22.43..24.98] | 26.37 [24.73..28.41] | 14.1% | 0.001 |
| session-16B write_ns | 17.22 [17.09..17.43] | 17.45 [17.27..17.61] | 1.4% | 0.003 |
| session-16B read_ns | 26.30 [26.03..27.35] | 27.29 [26.27..27.89] | 3.7% | 0.004 |
| session-1024B write_ns | 29.71 [28.59..31.33] | 29.82 [28.91..31.77] | 0.4% | 0.821 |
| session-1024B read_ns | 32.81 [31.98..33.99] | 34.64 [32.45..35.61] | 5.6% | 0.007 |
| datagram-64B write_ns | 92.73 [91.33..93.92] | 88.61 [87.08..93.56] | -4.4% | 0.002 |
| datagram-64B read_ns | 104.0 [102.7..112.9] | 114.1 [104.7..121.2] | 9.7% | 0.003 |
| datagram-1200B write_ns | 197.9 [195.7..200.5] | 194.4 [192.1..195.8] | -1.8% | 0.000 |
| datagram-1200B read_ns | 189.6 [185.9..193.1] | 192.2 [188.7..197.0] | 1.4% | 0.049 |
| stream-1024B write_ns | 177.1 [175.9..178.9] | 172.0 [169.6..174.2] | -2.9% | 0.000 |
| stream-1024B read_ns | 144.2 [143.1..145.3] | 144.9 [143.4..147.9] | 0.5% | 0.273 |
| stream-8192B write_ns | 935.2 [919.9..971.3] | 952.9 [931.9..971.9] | 1.9% | 0.096 |
| stream-8192B read_ns | 753.1 [744.8..772.9] | 756.8 [751.2..775.0] | 0.5% | 0.597 |

Verdict: kept. Encrypted TCP legs answer twice as fast and no metric is
worse.

### u02-19: one HKDF extraction per root, not per derivation (kept, 36a3c1fd)

A temporary key is HKDF-SHA256 of the administrator key, salted with the
instance id, expanded with the key id. The relay derives one twice for each
new connection of a temporary key (`derive_key` to open the first flight,
`authenticate_presented` to check it) and once for each credential it
reveals, running the extraction every time, though its output changes only
when a root rotation or a reset replaces the key or the id. The extraction
(`temporary_root`) is now kept in `AuthStateInner`. The reset and the
rotation replace it together with the id or the key: they take its lock
first and write both before releasing it, so no derivation pairs a new key
or id with the old extraction. `derive_temporary_key` keeps its signature
and output; the reset and rotation tests now also assert that the kept
extraction derives what the full derivation does.

`auth_bench` (new), before = pb-mapper-auth as 03d1dd0f has it, after =
36a3c1fd, 10 rounds each; each run prints the median of 9 samples of
200,000 calls:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| derive_temporary_key ns | 356.6 [348.1..369.2] | 122.7 [120.9..130.1] | -65.6% | 0.000 |
| authenticate_temporary ns | 497.1 [489.9..506.7] | 266.1 [264.2..271.6] | -46.5% | 0.000 |
| authenticate_administrator ns | 120.1 [119.6..120.8] | 124.6 [124.3..125.6] | 3.7% | 0.000 |

Verdict: kept: `derive_key` is 66% cheaper and `authenticate_presented` 46%
for a temporary key, and a new temporary-key connection calls each once.
The administrator row, a control whose code did not change, moved 4.5 ns,
code and layout placement.

### The desktop host, measured with host_send_bench

`host_send_bench` (new) runs a relay, a host of an H.264 desktop whose
"encoder" returns one 4 KiB unit per capture, and a viewer whose renderer
presents every unit, all in one process and over the relay, at the viewer's
60 fps. After 2 s of warm-up it counts every heap allocation and
reallocation for 5 s of video alone (`video`), then for 5 s of video while
the viewer moves the pointer every 4 ms under control (`video+pointer`).
The count covers the whole process, viewer and relay included, so it
compares runs of this bench only; it barely varies between runs, which is
what makes one allocation per unit visible. B-send, the credit ledger and
thread C are `pub(crate)`, so their time cannot be taken alone: allocations
are the measure.

Each comparison changed only its own files; what else was uncommitted at
the time (the one-write framing and the auth change, and for u09-11 and
u09-13 each other) was in both builds.

#### u09-12: the ack queue's median in a reused buffer (kept, bf6652e1)

`CreditLedger::ack_queue` collected the recent receipt delays into a new
`Vec` and sorted it on every admission check (each unit, each 20 ms recheck
of a waiting head, each probe echo, each `Stats`). It now refills a buffer
the ledger keeps and takes the middle with `select_nth_unstable`, the value
the sort put there.

before = 967847e1, after = bf6652e1, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| video units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video allocs_per_s | 2073.5 [2071.0..2077.0] | 2008.5 [2006.0..2010.0] | -3.1% | 0.000 |
| video allocs_per_unit | 34.57 [34.52..34.63] | 33.48 [33.43..33.51] | -3.2% | 0.000 |
| video+pointer units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video+pointer inputs_per_s | 249.9 [249.9..249.9] | 249.9 [249.9..250.0] | 0.0% | 0.131 |
| video+pointer allocs_per_s | 6241.0 [6238.0..6244.0] | 6174.0 [6165.0..6180.0] | -1.1% | 0.000 |
| video+pointer allocs_per_unit | 104.0 [104.0..104.1] | 102.9 [102.8..103.0] | -1.1% | 0.000 |

Verdict: kept: one allocation fewer per unit admitted, 65 a second at
60 fps, with and without input.

#### u09-11: B-send keeps the engine state instead of cloning it every pass (kept, cbcf2054)

B-send cloned the whole `EngineState` out of its watch on every pass (each
unit pushed, each receipt, each cursor report, each state change), and
`for_viewer` cloned the record again even when it changed nothing. Now
`Sender` keeps the last state and copies the watch's value only when it
differs from it, and `for_viewer` returns a `Cow`, borrowed when the
viewer's record is the engine's. The same `State` and notices go out in the
same order.

The first version copied only when the watch's `has_changed` said so; its
first A/B run caught it never showing a picture (21 units in 10 s), because
`wait` marks a new value seen through `changed` before the next pass looks.
It now compares the values, which holds however the receiver was woken.
The runs of the first version, and some that by mistake overlapped another
bench loop, were discarded; the table is a clean rerun of the final code.

before = pb-mapper-desktop as bf6652e1 has it plus the u09-13 change below,
after = the same plus this change, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| video units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video allocs_per_s | 2008.5 [2006.0..2011.0] | 1761.0 [1759.0..1765.0] | -12.3% | 0.000 |
| video allocs_per_unit | 33.47 [33.44..33.53] | 29.35 [29.32..29.41] | -12.3% | 0.000 |
| video+pointer units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video+pointer inputs_per_s | 249.9 [249.9..250.0] | 249.9 [249.9..250.0] | 0.0% | 0.450 |
| video+pointer allocs_per_s | 5916.5 [5914.0..5921.0] | 5668.5 [5665.0..5673.0] | -4.2% | 0.000 |
| video+pointer allocs_per_unit | 98.65 [98.60..98.70] | 94.50 [94.44..94.57] | -4.2% | 0.000 |

Verdict: kept: 4.1 allocations fewer per unit, 248 a second at 60 fps, with
and without input.

#### u09-13: thread C reuses its window and lane buffers (kept, f641b573)

`Window::roll` ended with `*self = Self::new(now)`, freeing the input-wait
and encoder-dwell sample buffers every second for the next samples to
allocate again. It now clears them in place (destructuring `Self`, so a new
field must be reset there too). The input lane's `drain` handed its queue's
allocation to thread C to free, so the next input B-recv queued allocated
again; `drain_into` swaps the lane's queue with an emptied buffer thread C
keeps, for both the normal drain and the overflow drain. A cancelled drain
still drops, never runs, what is left. Published values and the order of
commands are unchanged.

before = pb-mapper-desktop as bf6652e1 has it plus the u09-11 change above,
after = the same plus this change, 10 rounds each:

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| video units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video allocs_per_s | 1759.5 [1759.0..1761.0] | 1760.0 [1758.0..1765.0] | 0.0% | 0.850 |
| video allocs_per_unit | 29.34 [29.31..29.36] | 29.34 [29.31..29.41] | 0.0% | 0.970 |
| video+pointer units_per_s | 60.00 [60.00..60.00] | 60.00 [60.00..60.00] | 0.0% | 1.000 |
| video+pointer inputs_per_s | 249.9 [249.9..250.0] | 249.9 [249.9..250.0] | 0.0% | 0.450 |
| video+pointer allocs_per_s | 5922.5 [5920.0..5925.0] | 5667.5 [5663.0..5669.0] | -4.3% | 0.000 |
| video+pointer allocs_per_unit | 98.73 [98.68..98.77] | 94.48 [94.40..94.51] | -4.3% | 0.000 |

Verdict: kept: with the pointer moving, 255 allocations a second fewer, about
one per input event; with no input and no encoder dwell to sample, as here,
nothing changes.

### Looked at and not taken

- **u03 report-only: cache the 32-byte key fold of session-keyed frames.**
  After u01-05 the process-keyed path, whose hash is precomputed, and the
  session-keyed path differ only by that fold: in `framing_bench` (16 B
  frames, the u01-05 and datagram sessions above) a session-keyed write takes
  17.2 ns against 8.6 ns, a read 26.1 ns against 14.6 ns: 8.6 and 11.5 ns a
  frame, against 92-940 ns to write an encrypted one and a send per frame
  on every leg, for a new public checksum API in pb-mapper-core. Not taken;
  the numbers are here for whoever weighs it again.
- **u03 report-only: plain datagram frames and v2 control messages still go
  out as three writes.** One write needs a copy of the payload (what the
  codec writer already has) or vectored writes, and no bench here exercises
  a plain UDP tunnel or a control exchange. Not taken; unmeasured.
- **u03 report-only: the per-frame `msg.to_vec()` in the codec writer.** The
  one-write change keeps exactly one allocation per frame (now holding the
  whole frame). Reusing a buffer across frames would keep up to the largest
  frame, 8 MiB, per stream for its lifetime. Not taken.

## Noise checks

### A/A: one binary on both sides

`relay_transport_bench` at 03d1dd0f against itself, 6 rounds, every run
pinned to CPUs 0-15 as it started:

| metric | A (n=6) | A2 (n=6) | change | p |
|---|---|---|---|---|
| Tcp codec=false MiB/s | 61.00 [41.40..69.30] | 46.00 [29.00..59.60] | -24.6% | 0.055 |
| Tcp codec=false p50 us | 151.9 [145.1..206.3] | 230.8 [156.1..345.2] | 52.0% | 0.025 |
| Tcp codec=false p95 us | 236.3 [222.1..261.5] | 333.2 [301.8..476.6] | 41.0% | 0.004 |
| Tcp codec=false p99 us | 352.2 [281.2..436.5] | 545.5 [396.4..671.0] | 54.9% | 0.010 |
| Tcp codec=true MiB/s | 51.60 [44.80..61.30] | 45.20 [32.50..59.60] | -12.4% | 0.200 |
| Tcp codec=true p50 us | 415.1 [403.0..483.4] | 396.1 [377.0..656.9] | -4.6% | 0.200 |
| Tcp codec=true p95 us | 528.0 [477.4..698.9] | 541.3 [447.2..844.0] | 2.5% | 1.000 |
| Tcp codec=true p99 us | 754.3 [644.8..992.1] | 724.2 [536.3..1140.4] | -4.0% | 0.522 |
| Quic codec=false MiB/s | 62.80 [50.10..70.90] | 58.65 [51.90..68.50] | -6.6% | 0.522 |
| Quic codec=false p50 us | 264.9 [228.1..323.8] | 274.8 [245.3..401.2] | 3.8% | 0.522 |
| Quic codec=false p95 us | 382.7 [342.7..496.4] | 418.7 [354.7..602.6] | 9.4% | 0.522 |
| Quic codec=false p99 us | 488.9 [394.3..674.5] | 531.0 [462.0..832.2] | 8.6% | 0.262 |
| Quic codec=true MiB/s | 52.65 [47.30..60.90] | 51.75 [39.00..57.20] | -1.7% | 0.631 |
| Quic codec=true p50 us | 250.7 [230.6..318.3] | 271.8 [242.5..332.8] | 8.4% | 0.522 |
| Quic codec=true p95 us | 405.1 [311.9..501.7] | 451.1 [380.7..536.9] | 11.4% | 0.200 |
| Quic codec=true p99 us | 465.1 [364.8..705.2] | 588.0 [525.2..654.5] | 26.4% | 0.262 |

Pinned runs spread as widely as the unpinned sessions above (TCP throughput
29-69 MiB/s here, 10-76 over part 1), so the other sessions ran unpinned.

## Files

- `part1-relay_transport_bench.txt`, `part1-data_codec_bench.txt`,
  `part1-galaxy_bench.txt`: part 1.
- `u05-02-relay_transport_bench.txt`, `u01-05-framing_bench.txt`,
  `datagram-copy-framing_bench.txt`, `one-write-relay_transport_bench.txt`,
  `one-write-framing_bench.txt`, `u02-19-auth_bench.txt`,
  `u09-12-host_send_bench.txt`, `u09-11-host_send_bench.txt`,
  `u09-13-host_send_bench.txt`: part 2, one per comparison.
- `aa-check-relay_transport_bench.txt`: the A/A check.
- `open-items-auth-*.txt`, `open-items-relay-*.txt` and
  `open-items-desktop-*.txt`: the raw runs of the three open-items records,
  each listed in its record.
- `stall-final-relay_transport_bench.txt`: the final stall comparison, base
  3207797 against 5ab03df8, 200 rounds each (see "Stalls in
  relay_transport_bench").
