# A standby session on the other path

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees were building on the same machine (load
averages of about 1.7 to 2.4 while these ran). Everything runs in one
process: an in-process relay on loopback, a host whose synthetic H.264
encoder makes a 64-byte unit per capture and notes when it encodes an IDR,
and a viewer whose renderer presents every unit at once. None of it is a
real network; see the last section.

```
cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture \
  --test-threads=1 slow_link::standby
```

## A dead path while the relay lives

`slow_link::standby_takes_a_dead_paths_place`, built at `a95730d`. The
viewer reaches the relay through the testkit's shaped link A, relay only; a
test hook opens its standby through link B, another hop to the same relay. Both are relayed, so
blocking A emulates a failure of the live session's own path while the
other path lives, as UDP blocked mid-session, a NAT that rebinds or a lost
port mapping would for a direct session beside a relay standby. Six seconds
after the viewer began connecting, with control granted, link A starts
holding every byte both ways. Per run: from the block to the first picture
read after it (from the standby's session), to input taken again (control
granted and a fresh picture of the new session's generation, which
`Viewer::input` accepts), and the IDRs the host encoded between the block and
100 ms before that picture (`keyframes_in_outage`) and from then to a second
after it (`keyframes_at_switch`). Ten runs; every line is in
[dead-path.txt](dead-path.txt).

| | p50 | p90 | max |
|---|---|---|---|
| block to the first picture, ms | 1568 | 1585 | 1585 |
| block to input taken, ms | 1572 | 1590 | 1591 |

Every run encoded one IDR during the outage and one at the switch. The one
during the outage is the dead session's: its queue at the host ages past
1 s with no receipt coming back, and the host replaces its head with an IDR,
as for any viewer whose path stalls. The one at the switch is the standby's
resume. The viewer state stayed `Streaming` throughout, and the host listed
one viewer.

The silence a live connection is given is `SILENCE_MIN`, 1.5 s, on this
path (four base round trips are far less), looked at every 100 ms; the
picture followed it by 67 to 85 ms.

## Baseline: no standby

`slow_link::standby_baseline_reconnects` (built at `a95730d`), the same rig
and block with no standby, and the same case built at the base commit
`5243c17` with only the testkit's blocked flag applied ([baseline-at-base.rs](baseline-at-base.rs),
appended to `slow_link.rs` there). The session ends at the viewer's read
timeout; a reconnect over link B is then timed, to its first picture and to
input taken. An app waits its reconnect backoff between the two, 1 s at
first, which neither counts.

| | at `5243c17` p50 / p90 / max | this branch, no standby p50 / p90 / max |
|---|---|---|
| block to the session ending, ms | 3991 / 4007 / 4008 | 3994 / 4009 / 4009 |
| reconnect to its first picture, ms | 28 / 29 / 30 | 26 / 29 / 50 |
| reconnect to input taken, ms | 57 / 61 / 61 | 59 / 65 / 91 |

Every session ended with `Timeout`. Lines: [baseline-at-base.txt](baseline-at-base.txt)
and [baseline-standby-off.txt](baseline-standby-off.txt). The reconnect here
is relay only to an in-process relay; a real one also spends its direct
attempts (up to 3 s to a grant and 2 s to punch, unless a pause skips them)
and a TLS handshake over the real path. On the host, a viewer silent for 3 s
ends; a host whose only viewer it was empties its room, which pauses capture
and puts a changed display mode back before the reconnect.

## A relayed session moving to direct

`slow_link::standby_upgrades_a_relayed_session`, built at `1c9a732`. An
in-process relay with loopback direct coordination, a host that allows direct, and a viewer that connects relay
only while a test hook says direct fell back from it, which stands in for a
transient fallback without the client's pause: a real fallback that says the
path will not punch pauses direct for 10 s first, and the standby waits it
out. Per run: from the session's first picture to its route reading direct,
the IDRs the host encoded from that picture to a second after the switch,
and the longest the renderer went without a picture within a second either
side of it. Then, with a relay standby open beside the direct session (this
loopback link is not on a LAN, so it gets one), the bytes the relay carried
for it both ways over 5 s, as the host's tunnel counts them: the desktop
protocol's TLS records, without the relay's own framing or TCP. Ten runs;
every line is in [upgrade.txt](upgrade.txt).

| | p50 | p90 | max |
|---|---|---|---|
| first picture to direct, ms | 119 | 125 | 129 |
| longest gap between pictures, ms | 33 | 34 | 34 |
| relay standby, bit/s both ways | 6798 | 6798 | 6800 |

Every run encoded exactly one IDR, the promoted session's resume. The gap
never exceeded the synthetic host's frame interval here. The standby's
traffic is its host session's stats once a second and its probes, which on
a path under 100 ms run every 250 ms, with their echoes, and the viewer's
pings; on a path of 100 ms or more the probes run once a second.

## The whole slow-link suite

At `e30344a` the whole suite, these three cases included, passed: 30 cases
in 1235 s, every line in [slow-link-suite.txt](slow-link-suite.txt). Its
three standby summaries match the runs above within 10 ms, with the same
IDR counts: block to picture p50 1569 ms, block to input p50 1574 ms, one IDR in
the outage and one at the switch; a session without a standby ending at
p50 4001 ms; first picture to direct p50 120 ms with one IDR, a longest gap
of 34 ms and 6798 bit/s on the relay standby.

At `12eb3d7`, after review fixes (a retired connection's writer let go of
the session's wakes, among them), the three standby cases ran once more,
every line in [final.txt](final.txt): block to picture p50 1568 ms (max
1585), block to input p50 1572 ms (max 1589), one IDR in the outage and one
at the switch; without a standby, ending at p50 3993 ms; first picture to
direct p50 118 ms (max 122) with one IDR, a longest gap of 34 ms and
6798 bit/s on the relay standby.

## What a device still has to show

- A Windows host and macOS, Windows and Linux viewers across two real NATs:
  a firewall rule on the host that drops UDP mid-session, watched from the
  block to the picture, control and the host's room (no restore of a changed
  display mode).
- UDP blocked for the first 5 s of a session, then let through: the session
  starts on the relay, waits the client's pause, and moves to direct.
- A viewer's own network change (Wi-Fi to cellular) kills both
  connections: a standby does not help there, and the app reconnects as
  before.
