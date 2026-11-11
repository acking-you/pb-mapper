# Open items in pb-mapper-auth: persistence off the runtime, and the idle snapshot

The two performance entries for pb-mapper-auth in
[the refactor's open items](../../refactor-2026-11-open-items.md), done after
its correctness fixes (634f158e). The method and the tables are those of
[README.md](README.md); only measured figures are below, and the raw runs are
in the files listed at the end.

## Setup

- Windows runs: the machine in [README.md](README.md). The state directories
  were under `%TEMP%` on C:, NTFS on an NVMe SSD. Another agent compiled in
  another checkout throughout: each timed run waited while any `rustc`,
  `cargo` or `link` process existed, and the two builds alternated, A then B
  on even rounds and B then A on odd ones.
- Linux runs: Ubuntu 24.04 in WSL2 on the same machine (kernel
  6.6.87.2-microsoft-standard-WSL2), ext4, strace 6.8.
- Kinds of number: latencies and holds are times, which load moves;
  snapshot rewrites, bytes written and syncs are counts, which it does not.
- Each table gives `median [min..max]` over the runs of each side, the
  change of the median and a two-sided Mann-Whitney U p-value.

## Persistence off the runtime's workers (kept, 7abb8cd2)

### What ran where

The actor was a tokio task, and each command ran in whichever worker polled
it. So that worker, and on a current-thread runtime every task, waited for:
an administrator mutation's WAL append (open, `fchmod`, two writes,
`fdatasync`, a directory sync when the file is new), which a claim, a show,
a reveal and an audit also make; a compaction, every five minutes or for a
GC (serialize the whole snapshot, seal it, write and `fsync` a temporary
file, rename it, sync the directory, truncate and sync the WAL), and a reset
or a rotation; and a start's file work and table building, done inside the
`async fn` constructors.

Now `Actor::run` keeps the loop, the one-second tick and the schedule's
advance, which touches only memory. Every command and every compaction is a
step that runs on the blocking pool (`spawn_blocking`) with the actor moved
into it and handed back; the loop awaits each step before it takes the next.
A start loads and builds on the blocking pool too, then spawns the actor on
the caller's runtime. The helper that runs both (`off_runtime`) logs to the
caller's tracing dispatcher on the blocking thread and resumes a panic in
the caller, as if the work had run inline.

### Why this shape, not a dedicated thread

Both keep the actor's code as it was, one step after another. This one also
keeps the actor a task of the caller's runtime:

- `shutdown_actor` and `abort_actor` work through the task's handles
  unchanged. An abort, or a runtime shutdown, lets a step under way finish
  and drops the actor after it, as it waited for the worker's poll before;
  a step not yet started is dropped unrun, as a command still queued was.
- Dropping a tokio runtime waits for its blocking tasks, so a program that
  returns from `main` without `shutdown_actor` still lets a WAL append
  finish. A thread of its own would be cut off at exit, and a torn last WAL
  record fails the store closed at the next start.
- With the clock paused (`start_paused`), tokio does not auto-advance while
  a `spawn_blocking` task runs; it would while a thread of its own worked.

What this shape costs: a step needs a free blocking-pool thread, so with
every one busy (handshakes queued behind the replay log's fsync, for one)
commands and compactions queue behind them. The tick stays on the task, so
a key's expiry still ends its sessions on time.

### The guarantees

- Order: commands come off one channel and each step finishes, its WAL
  append included, before the next begins. A new test sends 32 renewals of
  one key at once and checks that a restart restores the deadline the relay
  held (`concurrent_mutations_persist_in_the_order_they_took_effect`).
- An answer only after its record is durable: the step sends it, after
  `sync_data`, as before; `issue_renew_revoke_and_persist` reads the files
  right after the call.
- Failures: the same code runs inside the step, so safe mode, the
  cancellations and the log lines are as before.
- The state lock: the actor holds it and moves with the step, so it is never
  let go earlier than before.
- Dropping the last handle closes the channel; a last step stops the actor
  and drops it (`overlapping_runtimes_cannot_share_an_auth_state_directory`).
- Abort and shutdown: unchanged, and their tests pass unchanged.

### Measured

`auth_bench` gained cases that run on a current-thread runtime (`_current`)
and on a four-worker one (`_multi`), driven from a task as a relay's
administrator connection drives them: 200 renewals, one durable WAL append
each (`renew`); 200 status requests, a command with no file work
(`status`); 20 GCs, which write the snapshot a compaction writes after a
sweep of the slot table, at the default capacity of 65,536 and at the
largest, 1,048,576, with 100 keys (`gc65536`, `gc1048576`); and 20 starts on
that state (`start65536`, `start1048576`). Busy probe tasks, one per worker,
yield in a loop and record the longest gap between two of their turns; an
operation's hold is the longest gap that ended while it ran or just after
(`hold`), and its round trip is `latency`. Each run prints, per case, the
median of its operations' figures (`_p50`) and the largest (`_max`).
Before, the hold equals the operation's latency when the actor shares the
probe's worker, and it grows from the default capacity to the largest: the
measure responds to its input.

Before = 634f158e with the bench, after = 7abb8cd2, two sessions of 20
rounds pooled (the first without the `status` case):

| metric | before (n=40) | after (n=40) | change | p |
|---|---|---|---|---|
| renew_current latency_p50_us | 712.0 [631.4..1031.2] | 725.0 [684.9..883.8] | 1.8% | 0.001 |
| renew_current hold_p50_us | 717.5 [635.7..1042.9] | 3.400 [3.200..5.500] | -99.5% | 0.000 |
| renew_current hold_max_us | 1319.9 [912.2..9445.8] | 46.15 [8.100..2395.3] | -96.5% | 0.000 |
| gc65536_current latency_p50_us | 3500.3 [3106.9..6449.1] | 3542.6 [3246.6..4075.9] | 1.2% | 0.281 |
| gc65536_current hold_p50_us | 3509.1 [3114.6..6467.8] | 5.900 [4.400..48.00] | -99.8% | 0.000 |
| gc65536_current hold_max_us | 5896.9 [3745.4..10174.1] | 33.50 [7.700..2095.4] | -99.4% | 0.000 |
| start65536_current latency_p50_us | 1516.8 [1456.7..3353.6] | 1616.1 [1453.9..1941.5] | 6.5% | 0.000 |
| start65536_current hold_p50_us | 1524.2 [1462.4..3385.3] | 5.250 [3.600..10.00] | -99.7% | 0.000 |
| start65536_current hold_max_us | 2925.6 [1733.9..52373.5] | 16.60 [6.700..631.1] | -99.4% | 0.000 |
| gc1048576_current latency_p50_us | 12446.8 [11648.8..21499.4] | 12447.1 [11737.6..21199.1] | 0.0% | 0.923 |
| gc1048576_current hold_p50_us | 12457.7 [11658.9..21512.6] | 8.450 [6.200..14.00] | -99.9% | 0.000 |
| gc1048576_current hold_max_us | 14294.9 [12490.8..41635.3] | 65.60 [10.90..2290.2] | -99.5% | 0.000 |
| start1048576_current latency_p50_us | 13016.3 [12188.6..26482.6] | 13272.0 [12409.7..13613.2] | 2.0% | 0.003 |
| start1048576_current hold_p50_us | 13038.3 [12210.5..26528.9] | 14.95 [9.400..21.70] | -99.9% | 0.000 |
| start1048576_current hold_max_us | 14374.4 [12550.0..31477.2] | 97.55 [19.20..2881.6] | -99.3% | 0.000 |
| renew_multi latency_p50_us | 744.9 [680.8..1157.9] | 768.2 [721.3..874.4] | 3.1% | 0.021 |
| renew_multi hold_p50_us | 4.400 [3.800..43.60] | 6.100 [4.400..12.60] | 38.6% | 0.000 |
| renew_multi hold_max_us | 162.9 [27.50..1778.7] | 185.4 [26.30..2405.3] | 13.8% | 0.085 |
| gc65536_multi latency_p50_us | 3696.2 [3451.1..6453.9] | 3722.8 [3349.9..4001.3] | 0.7% | 0.981 |
| gc65536_multi hold_p50_us | 3698.9 [3453.1..6460.4] | 13.95 [6.900..126.0] | -99.6% | 0.000 |
| gc65536_multi hold_max_us | 5778.9 [4116.3..10892.3] | 168.7 [20.40..378.3] | -97.1% | 0.000 |
| start65536_multi latency_p50_us | 1644.1 [1521.8..3888.6] | 1700.2 [1527.1..2083.8] | 3.4% | 0.007 |
| start65536_multi hold_p50_us | 1649.3 [1526.2..3903.1] | 11.25 [5.700..82.40] | -99.3% | 0.000 |
| start65536_multi hold_max_us | 3032.8 [2102.5..10818.6] | 147.9 [13.20..1030.6] | -95.1% | 0.000 |
| gc1048576_multi latency_p50_us | 12786.3 [12022.0..24729.8] | 12670.8 [11810.7..22471.9] | -0.9% | 0.476 |
| gc1048576_multi hold_p50_us | 12789.2 [12027.8..24733.8] | 32.65 [9.200..171.2] | -99.7% | 0.000 |
| gc1048576_multi hold_max_us | 17416.0 [12644.9..34517.8] | 213.3 [58.10..1815.0] | -98.8% | 0.000 |
| start1048576_multi latency_p50_us | 13259.3 [12351.5..21733.3] | 13410.5 [12415.2..20310.5] | 1.1% | 0.923 |
| start1048576_multi hold_p50_us | 13268.3 [12360.9..21745.4] | 38.10 [12.60..212.6] | -99.7% | 0.000 |
| start1048576_multi hold_max_us | 18865.7 [12963.8..27794.7] | 251.4 [108.1..3289.7] | -98.7% | 0.000 |
| derive_temporary_key ns | 123.5 [121.2..135.5] | 124.3 [121.0..129.3] | 0.6% | 0.183 |
| authenticate_temporary ns | 269.6 [266.0..292.8] | 265.0 [261.0..274.7] | -1.7% | 0.000 |
| authenticate_administrator ns | 125.8 [124.8..139.5] | 126.4 [125.3..128.5] | 0.4% | 0.003 |

The second session alone, for the `status` case:

| metric | before (n=20) | after (n=20) | change | p |
|---|---|---|---|---|
| status_current latency_p50_us | 14.70 [14.10..24.40] | 29.80 [28.10..30.60] | 102.7% | 0.000 |
| status_current hold_p50_us | 16.00 [15.70..25.80] | 2.700 [2.600..2.900] | -83.1% | 0.000 |
| status_multi latency_p50_us | 15.30 [14.50..25.30] | 30.35 [29.40..30.90] | 98.4% | 0.000 |
| status_multi hold_p50_us | 1.700 [1.600..2.700] | 2.600 [2.400..3.900] | 52.9% | 0.000 |

An A/A check, the before build against a copy of itself, 20 rounds:

| metric | A (n=20) | A again (n=20) | change | p |
|---|---|---|---|---|
| renew_current latency_p50_us | 700.8 [670.8..1222.4] | 708.7 [673.6..792.4] | 1.1% | 0.239 |
| status_current latency_p50_us | 14.60 [14.10..25.10] | 14.45 [14.10..24.50] | -1.0% | 0.229 |
| gc65536_current latency_p50_us | 3442.9 [3235.3..6463.0] | 3499.9 [3315.0..3984.2] | 1.7% | 0.291 |
| start65536_current latency_p50_us | 1537.9 [1424.1..3297.1] | 1539.5 [1445.1..2407.8] | 0.1% | 0.579 |
| renew_multi latency_p50_us | 732.8 [688.1..1110.3] | 732.2 [681.1..905.4] | -0.1% | 0.797 |
| status_multi latency_p50_us | 15.60 [14.60..25.50] | 15.45 [15.10..25.20] | -1.0% | 0.725 |

What the tables say:

- What the item is for: the time other tasks on the runtime wait. A renewal
  held them for its whole round trip on a current-thread runtime (median
  718 us), a GC for 3.5 ms at the default capacity and 12.5 ms at the
  largest, a start for 1.5 and 13 ms; on a multi-thread runtime the GC and
  the start held the probe on the actor's worker as long. Now each holds
  them for 3 to 38 us at the median. On the multi-thread runtime a renewal
  held them for 4.4 us before, its actor already on another worker, and
  6.1 us now.
- What it costs: one hand-off to a blocking-pool thread and back per step.
  A status request, a command with no file work, took 14.7 us and takes
  29.8 us; in the next session (below), on the same code path, it took
  20.3 us, so the hand-off costs 6 to 15 us here. A renewal, which is that
  plus one `fdatasync`, takes 13 us (1.8%, current-thread) and 23 us (3.1%,
  multi-thread) longer at the median, outside the A/A check's spread of
  about 1%. Starts take 1.1 to 6.5% (0.06 to 0.26 ms) longer at the
  median, beyond chance in three of the four cases; GCs do not move.
- The hot path does not move: derivation and authentication stay within
  2%, the code-placement noise README.md describes.

Verdict: kept. Stalls of every other task on the runtime for each
administrator mutation, compaction and start, 0.7 to 13 ms at the median,
become stalls of a few microseconds, for one thread hand-off per
administrator command. Any form of the change pays that hand-off, since the
work has to leave the caller's thread. It adds 13 to 23 us to a renewal
here: beyond this machine's noise, and a fixed cost beside the renewal's
own fsync. An administrator command is an operator's request; no tunnel's
traffic passes through the actor.

## The snapshot rewritten when nothing changed (kept, fe6d4be5)

### The first pass's argument, checked

The claim: an append since the last compaction is the whole dirty
condition, because a restart recomputes every timer-driven change from the
snapshot. What a restart computes, if the last snapshot predates the change
and the WAL is empty:

- A key that expired at its deadline `E`: the snapshot has it active with
  `expires_at = E`. `restore_slots` (and `restore_high_slots` for a row
  above the capacity) marks an active key with `expires_at <= now` expired,
  with `tombstoned_at = E`, as `retire` did; `Leases::restored` schedules
  its reap at `E + 60`, at once if that has passed.
- A key that was reaped: `Leases::restored` reaps a dead row whose retention
  has passed while it schedules, so the row comes back free with its
  generation, which `generations` (or the fixed `high_slot_generations`)
  kept, as `reap` left it. The capacity accounting, the generations and
  which rows are free, comes out the same.
- A revoked key: the revoke is a WAL record (`StateMutation::Revoke { at }`),
  so no snapshot misses it; its reap is the case above.
- The replay window: `AdminReplays::restored` keeps the records within
  retention at the restart, as `prune` did.

Each of these holds only for a restart whose clock reads at or after the
second the change happened. The schedule the first pass just made keeps a
dead key dead whichever way the clock steps: the wheel only turns forward,
and a key that ran out is never live again. A restart does not: after the
clock steps back before `E`, `restore_slots` finds `expires_at > now` and
builds the key a live lease again, from a snapshot written before the
expiry; one written after it says expired, or free once reaped. The same
holds after a forward step that ended a key early and was then corrected,
and, in the safe direction, for a replay record pruned before such a step.
So the argument holds for a clock that never reads earlier at a restart
than at the change, and fails across a backward step. Before this change a
restart within the five minutes between such a change and the next
compaction revived the key too; skipping on the WAL alone would widen that
window to whenever the next administrator command comes.

### The change

The snapshot a compaction writes is a function of the state alone, so a
compaction with nothing changed since the last would write what is on disk,
under a new nonce. The store now keeps a flag, set by every change outside
a snapshot write: each WAL append (attempted, which also covers the
namespace mark a failed append leaves in memory), a key's retirement or
reap, the replay window letting a record go, and a start. The five-minute
compaction runs on the same schedule, and writes only when the flag is set;
an interval with nothing to write counts as a compaction. Whenever a
compaction is skipped the WAL is empty, so the WAL stays bounded as before.
The files' formats and contents are unchanged: 0.11.0 reads what this
writes, and the reverse.

A test (`a_compaction_writes_only_after_a_change_and_an_expiry_is_one`)
drives the actor with explicit seconds: the first compaction after a start
writes, an idle interval writes nothing, a key's expiry and reap make the
next one write, and after the clock steps back before the key's deadline a
restart from the files finds its row free, where the snapshot from before
the expiry would have brought the key back; a WAL append makes the next
compaction write and empty the WAL. Without the hooks in `retire` and `reap`
it fails at the expiry.

### Measured

`snapshot_bench` (new) starts a store at the default capacity with 100 keys
that outlive the run, asks nothing more of it, and counts how often
`auth.snapshot` is replaced and how many bytes that is; on Linux it also
reports what the process passed to `write` (`/proc/self/io`), and strace
counted the syncs. Before = 7abb8cd2, after = fe6d4be5, two hours each, on
Linux; the hours count from the end of the start and the 100 issues:

| idle hour | build | snapshot rewrites | snapshot bytes written | fsync calls |
|---|---|---|---|---|
| first | before | 12 | 1,818,396 | 36 |
| | after | 1 | 151,532 | 3 |
| second | before | 12 | 1,818,396 | 36 |
| | after | 0 | 0 | 0 |

Each compaction wrote a snapshot of 151,533 bytes at this capacity and
synced three times: the temporary file, the directory after the rename,
and the truncated WAL. The one write after the change is the compaction
five minutes after the start, which folds the 100 issues' WAL records into
the snapshot, as before. Over the two hours the processes passed 3,639,280
and 153,781 bytes to `write` (`/proc/self/io`); both counts include the
bench's own report line every five minutes.

The auth_bench cases do not move: item-1 build against this one, 10 rounds
each:

| metric | 7abb8cd2 (n=10) | fe6d4be5 (n=10) | change | p |
|---|---|---|---|---|
| renew_current latency_p50_us | 767.7 [729.0..877.9] | 773.6 [743.3..825.1] | 0.8% | 0.762 |
| renew_multi latency_p50_us | 797.0 [759.3..838.1] | 795.5 [760.0..853.5] | -0.2% | 1.000 |
| gc65536_current latency_p50_us | 3776.6 [3480.7..3990.5] | 3769.1 [3429.6..3925.2] | -0.2% | 0.762 |
| gc1048576_current latency_p50_us | 13095.1 [11950.9..21802.1] | 13164.8 [12213.2..21704.1] | 0.5% | 0.880 |
| start65536_current latency_p50_us | 1695.7 [1592.7..1853.0] | 1687.1 [1633.9..1828.0] | -0.5% | 1.000 |
| status_current latency_p50_us | 29.55 [27.50..30.70] | 20.30 [19.90..24.50] | -31.3% | 0.000 |
| status_multi latency_p50_us | 30.30 [20.30..31.70] | 20.95 [20.60..30.70] | -30.9% | 0.011 |

The change does not touch the status path. A status round trip falls near
20 us or near 30 us depending on the run, and in the multi-thread case each
build has runs of both kinds; whether code placement or scheduling decides
which is not established, and no change in it is claimed for this commit.

Verdict: kept. An idle relay at the default capacity no longer writes
1.8 MB and makes 36 fsyncs an hour; it writes the snapshot five minutes
after it starts and then only after a change. What any restart computes is
what it computed before: every compaction skipped would have written the
snapshot already on disk.

## Files

- `open-items-auth-item1-auth_bench.txt`: the two sessions of the first
  item (`item1`, `item1b`).
- `open-items-auth-aa-auth_bench.txt`: the A/A check.
- `open-items-auth-item2-auth_bench.txt`: the second item's auth_bench
  comparison.
- `open-items-auth-idle-snapshot_bench.txt`: the idle runs, with their
  syncs per hour as strace logged them.
