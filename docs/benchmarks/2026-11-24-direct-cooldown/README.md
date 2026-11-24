# Shared direct cooldown: connect_tcp time to the first byte

Release builds on a 12-core Apple M6 with 24 GiB RAM, macOS 27.0 (26A425) and
Rust 1.99.1. Other worktrees were building on the same machine while these
ran (a load average of about 5 during the after runs). Everything runs in one
process on loopback: a test relay that offers direct, a publisher that allows
it, and a client whose direct endpoints drop every QUIC packet they send (a
test-only fault), so each direct attempt spends the whole 2 s punch budget
and falls back to the relay before its commit point. That fallback is one
that pauses the client's next calls: a failed punch that the relay then
carried. Each figure is the time from calling `Client::connect_tcp` to the
first byte the echoing backend sends back. None of it is a real NAT; see the
last section.

## Runs

After (`be79de9`, this change, which pauses the client's next calls only
after a fallback that says the path cannot be punched and that the relay
carried):

```
cargo test --release -p pb-mapper-client --all-features --lib \
  direct::tests::a_fallback_pauses_direct_for_the_clients_next_connect_tcp \
  -- --nocapture --exact
```

Before (`1515eda` with [`baseline.patch`](baseline.patch), which adds the
same fault and the same timed calls, without anything new to assert):

```
cargo test --release -p pb-mapper-client --all-features --lib \
  direct::tests::baseline_connect_tcp_first_byte -- --nocapture --exact
```

Five runs each; medians below, every run in [after.txt](after.txt) and
[before.txt](before.txt).

| Call | `1515eda` | `be79de9` |
| --- | ---: | ---: |
| First `connect_tcp`, falling back | 2031.2 ms | 2028.3 ms |
| Next `connect_tcp` to the same service through the same client | 2025.1 ms (range 2022.7–2027.1) | 8.3 ms (range 7.8–8.7) |
| The one after a network change | — | 2027.2 ms |

Before, every call tried direct again (its diagnostics count one fallback
and no cooldown skip): each one paid the punch budget. After, the call
inside the pause skipped direct (`cooldown_skips` 1, no fallback) and took
the relay at once. A network change cleared the pause, so the call after it
tried direct again and fell back.

## What needs a device

The saving where it matters, a desktop viewer behind a symmetric NAT or
blocked UDP reconnecting to the same computer (time to the first picture per
reconnect), and whether sharing the consumer's endpoint and discovery as
well would save more (the setup timings of real connects), were not
measured here.
