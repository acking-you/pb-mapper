# Open items in the relay: replay admission, the re-subscribe poll, buffer retention

The three performance entries for pb-mapper-protocol and pb-mapper-server in
[the refactor's open items](../../refactor-2026-11-open-items.md), done after
the correctness fixes (634f158e) and the auth items
([open-items-auth.md](open-items-auth.md)). The method and the tables are
those of [README.md](README.md); only measured figures are below, and the raw
runs are in the files listed at the end.

## Setup

- Windows runs: the machine in [README.md](README.md). Another agent compiled
  in another checkout throughout: each timed run waited while any `rustc`,
  `cargo` or `link` process existed, and the two builds alternated, A then B
  on even rounds and B then A on odd ones.
- Disks: C: is a WD_BLACK SN850X 1 TB, D: an aigo NVMe SSD P7000E 2 TB, both
  NVMe and NTFS; `%TEMP%` is on C:. Linux runs: Ubuntu 24.04 in WSL2 on the
  same machine (kernel 6.6.87.2-microsoft-standard-WSL2), ext4 on WSL's
  virtual disk, strace 6.8.
- Kinds of number: rates, latencies and CPU times are times, which load
  moves; syncs, manager asks, log lines and bytes held are counts, which it
  moves little or not at all.
- Each table gives `median [min..max]` over the runs of each side, the change
  of the median and a two-sided Mann-Whitney U p-value.

## Replay-log admission (kept, b00c2156)

### What bounded it

A protocol-v2 first flight that decrypts is admitted against the relay's
replay log, `connection.replay`, before its request is read: register,
subscribe, stream, status and administrator connections alike, over TCP and
QUIC. Each admission ran on a blocking-pool thread under one relay-wide
lock, held across the append's open, write and `sync_data`, and once a
minute across a whole compaction. Admissions ran one sync at a time, and
each one waiting parked a blocking-pool thread.

The admission budget does not bound it first. A credential may open
`2 x PB_MAPPER_NEW_STREAMS_PER_SECOND x 2 x` the 600 s replay window first
flights a window, 240,000 at the default rate of 100 (400 a second on
average, all at once if it likes), and the relay as many in total. A
namespace's rate, 100 new streams a second in bursts of 200, is applied by
the routing manager after the first flight; first flights it then refuses
pass the lock too. A relayed connection costs two first flights, the
subscriber's and its provider's stream, so one namespace's burst of 200
streams is 400 first flights at once.

`handshake_bench` (new, pb-mapper-protocol) drives
`ServerSecurity::read_initial` as the relay's connection tasks do: `n` tasks
on a multi-thread runtime read 4,000 first flights of fresh sessions between
them, after 64 to warm up, against a state directory on the disk named. It
prints admissions a second and the latency of `read_initial` (median, 99th
percentile, largest).

Before, admissions a second did not move with concurrency, and latency grew
with it, one sync for each admission ahead (medians):

| state directory | 1 at once | 8 | 64 | 256 |
|---|---|---|---|---|
| C: (n=15) | 1,114/s, 0.87 ms | 1,085/s, 7.3 ms | 1,080/s, 59 ms | 1,077/s, 236 ms |
| D: (n=10) | 344/s, 2.9 ms | 343/s, 23 ms | 342/s, 187 ms | 342/s, 749 ms |
| WSL ext4 (n=6) | 198/s, 5.0 ms | 199/s, 40 ms | 201/s, 319 ms | 200/s, 1,281 ms |

The sync bounds it. On the WSL volume, whose sync takes about 5 ms as a slow
disk's does, the whole relay admitted 200 first flights a second, one
namespace's default stream rate; a burst of 256 waited 1.3 s.

### The change

- A first flight is checked and reserved under one lock: the Bloom filter
  and an exact set of the fingerprints being logged (`pending`), then the
  per-credential and total budgets, which now count reservations.
- One writer thread per relay (`pb-mapper-replay-log`) takes every
  reservation queued while it wrote the batch before (up to 4,096), appends
  them in one write and one `sync_data` (and a directory sync for a new
  file) with the same append function, and then, under the lock, moves each
  fingerprint from `pending` into the filter, or, if the append failed,
  drops it from `pending` and gives back what it took from the budgets.
- A failed append is rolled back to the length before the batch or latches
  the log unavailable, as a failed single append did; every first flight in
  the batch is refused `connection_replay_store_unavailable`.
- Compaction runs on the writer after the batch that comes due, before its
  answers, as it ran inside the admission that came due.
- The connection task awaits its answer; it no longer takes a blocking-pool
  thread. `authenticate_presented`, in memory, now runs on its worker,
  outside the replay lock.
- What is logged, and where: the writer logs to the tracing dispatcher in
  effect when the relay's security was made.

The files are unchanged: the same 40-byte records, appended at the end,
compacted to the live ones. A batch is one append of whole records, so a
crash in it can leave whole records nobody was answered for and one cut off
last; the cut-off one is dropped at the next start, as the first pass made
it, and the whole ones are kept, which refuses only salts no client was told
it may use.

### The properties, and their tests

Written first, against the old code, which passes them; the new code passes
them too, and each fails on a version of the change that gets it wrong
(a mutation, run once each):

| property | test (`secure/tests.rs`) | the mutation it catches |
|---|---|---|
| Fresh only once durable | `racing_first_flights_admit_each_fingerprint_once_and_durably`: each `Fresh` answer finds its record in the log | answering before the append |
| check-and-reserve atomic for equal fingerprints | the same: 32 first flights from 32 threads at once, 8 of them copies of one, every other one claimed rather than admitted, 8 rounds: each fingerprint `Fresh` once | no `pending` check |
| a failed batch refuses all in it and gives back what they took | `a_failed_append_frees_what_its_admissions_reserved`: 16 refused while the log's directory is blocked, then the same 16 `Fresh` with a budget of 16, the 17th `Limited`, the log holding just the 16 | not giving back |
| limits count reservations | `racing_first_flights_never_pass_the_admission_limits`: of 40 at once, exactly 5 per credential, 7 in total | not counting reservations |
| restart, torn tail included | the race test restarts on its log with a record cut off at the end: every admitted fingerprint `Replayed`, the cut-off one `Fresh` | |
| compaction between batches | `a_compaction_leaves_the_live_admissions_and_admits_after_it`: the admission that comes due finds the log holding exactly the live records, and admissions go on after it | |

The existing tests (identical first flights admitted once, revoked and
stale-root claims, rollback, the failure log once per run, record layout,
restart) pass unchanged in substance: they now wait for the answer, and the
failure-log test opens the guard under its log collector, since the writer
logs where the guard was opened.

### Measured

`handshake_bench`, before = b8529eee (the auth items) plus the bench, after =
b00c2156, on each state directory. The C: and D: runs used an after build
from before two lines of `reserve` were reordered (the fingerprint goes into
`pending` before, not after, the record is queued, under the same lock);
three later rounds of the final build on C:, under "Compaction", agree.

On C: (n=15 each):

| metric | before (n=15) | after (n=15) | change | p |
|---|---|---|---|---|
| c1 per_s | 1114.0 [976.0..1134.0] | 1110.0 [1072.0..1147.0] | -0.4% | 0.756 |
| c1 p50_us | 872.1 [862.8..909.5] | 880.3 [851.4..912.4] | 0.9% | 0.787 |
| c1 p99_us | 1362.0 [1327.0..6014.7] | 1355.4 [1327.4..1391.7] | -0.5% | 0.917 |
| c1 max_us | 4241.1 [3509.7..9490.2] | 3863.5 [3059.9..5599.7] | -8.9% | 0.254 |
| c8 per_s | 1085.0 [1041.0..1115.0] | 4792.0 [4645.0..4988.0] | 341.7% | 0.000 |
| c8 p50_us | 7295.9 [7150.6..7543.1] | 1643.0 [1584.8..1697.2] | -77.5% | 0.000 |
| c8 p99_us | 9000.2 [8301.5..12701.5] | 2213.6 [2027.7..2484.6] | -75.4% | 0.000 |
| c8 max_us | 11854.5 [10810.3..18356.5] | 5145.9 [2513.3..8387.9] | -56.6% | 0.000 |
| c64 per_s | 1080.0 [1049.0..1106.0] | 37536.0 [33442.0..39204.0] | 3375.6% | 0.000 |
| c64 p50_us | 59032.4 [57564.6..60618.4] | 1655.3 [1593.8..1869.3] | -97.2% | 0.000 |
| c64 p99_us | 64541.4 [61874.2..68796.3] | 2259.1 [2003.5..5407.5] | -96.5% | 0.000 |
| c64 max_us | 67396.4 [63690.7..121043.4] | 2294.3 [2045.1..5422.1] | -96.6% | 0.000 |
| c256 per_s | 1077.0 [1046.0..1108.0] | 136023.0 [122933.0..144246.0] | 12529.8% | 0.000 |
| c256 p50_us | 236264.7 [230275.9..245350.8] | 1790.5 [1685.0..1983.1] | -99.2% | 0.000 |
| c256 p99_us | 249843.1 [238899.2..269095.8] | 2383.6 [2077.5..2541.7] | -99.0% | 0.000 |
| c256 max_us | 259149.6 [239467.4..474090.9] | 2600.2 [2335.6..2951.5] | -99.0% | 0.000 |

On D: (n=10 each):

| metric | before (n=10) | after (n=10) | change | p |
|---|---|---|---|---|
| c1 per_s | 344.0 [330.0..374.0] | 343.5 [341.0..348.0] | -0.1% | 0.762 |
| c1 p50_us | 2945.2 [2930.0..2952.9] | 2946.3 [2942.1..2951.0] | 0.0% | 0.450 |
| c1 p99_us | 3174.7 [3156.4..3207.8] | 3176.1 [3158.6..3210.9] | 0.0% | 0.910 |
| c1 max_us | 5550.7 [4995.0..443101.0] | 6090.3 [5115.6..7051.7] | 9.7% | 0.151 |
| c8 per_s | 343.0 [340.0..389.0] | 1402.5 [1388.0..1458.0] | 308.9% | 0.000 |
| c8 p50_us | 23477.0 [20631.6..23944.6] | 5991.3 [5892.1..5999.1] | -74.5% | 0.000 |
| c8 p99_us | 25950.2 [24144.2..26100.4] | 6165.5 [6112.6..6937.8] | -76.2% | 0.000 |
| c8 max_us | 28059.0 [27000.5..29092.8] | 8038.8 [7050.7..10014.4] | -71.4% | 0.000 |
| c64 per_s | 342.0 [341.0..345.0] | 11015.5 [10767.0..11143.0] | 3120.9% | 0.000 |
| c64 p50_us | 187111.2 [185989.9..188029.7] | 5990.9 [5979.9..6013.3] | -96.8% | 0.000 |
| c64 p99_us | 192982.6 [190913.8..209990.4] | 6131.8 [6086.7..7906.4] | -96.8% | 0.000 |
| c64 max_us | 195606.3 [192073.0..212329.2] | 6226.4 [6131.8..8118.7] | -96.8% | 0.000 |
| c256 per_s | 342.0 [333.0..344.0] | 42867.0 [41085.0..43912.0] | 12434.2% | 0.000 |
| c256 p50_us | 749423.9 [743979.0..751965.7] | 5950.1 [5900.6..5977.8] | -99.2% | 0.000 |
| c256 p99_us | 764030.1 [753107.3..1097120.0] | 6249.5 [6178.6..9304.4] | -99.2% | 0.000 |
| c256 max_us | 765137.9 [755997.5..1099221.3] | 6461.0 [6338.7..9548.0] | -99.2% | 0.000 |

On WSL's ext4 (n=6 each):

| metric | before (n=6) | after (n=6) | change | p |
|---|---|---|---|---|
| c1 per_s | 197.5 [195.0..201.0] | 198.0 [197.0..200.0] | 0.3% | 0.522 |
| c1 p50_us | 5021.2 [5013.6..5026.2] | 5018.7 [5012.5..5025.0] | -0.0% | 0.471 |
| c1 p99_us | 6094.0 [6055.0..6141.3] | 6085.4 [6068.3..6093.5] | -0.1% | 0.337 |
| c1 max_us | 9088.8 [9027.9..11824.0] | 9061.2 [8938.3..9147.1] | -0.3% | 0.337 |
| c8 per_s | 198.5 [196.0..218.0] | 800.5 [791.0..807.0] | 303.3% | 0.004 |
| c8 p50_us | 40032.9 [38072.4..40832.6] | 10018.5 [10014.1..10036.6] | -75.0% | 0.004 |
| c8 p99_us | 43665.7 [43053.8..48038.2] | 11984.4 [11950.4..12105.1] | -72.6% | 0.004 |
| c8 max_us | 47097.6 [45145.4..119997.5] | 13559.4 [13032.4..15043.2] | -71.2% | 0.004 |
| c64 per_s | 200.5 [197.0..202.0] | 6367.5 [6283.0..6519.0] | 3075.8% | 0.004 |
| c64 p50_us | 318996.7 [316996.3..325109.9] | 10000.0 [9990.1..10004.3] | -96.9% | 0.004 |
| c64 p99_us | 326535.4 [325010.9..336990.9] | 11101.0 [11034.7..12970.0] | -96.6% | 0.004 |
| c64 max_us | 329539.3 [326900.9..344101.5] | 11544.8 [11078.0..13062.3] | -96.5% | 0.004 |
| c256 per_s | 199.5 [190.0..201.0] | 24736.0 [23938.0..24929.0] | 12299.0% | 0.004 |
| c256 p50_us | 1281496.3 [1272924.8..1315113.1] | 10013.0 [9951.4..10057.7] | -99.2% | 0.004 |
| c256 p99_us | 1307611.5 [1284980.7..1822177.3] | 11120.2 [10398.0..14257.0] | -99.1% | 0.004 |
| c256 max_us | 1310090.6 [1286289.2..1824124.9] | 11304.5 [10780.2..14407.0] | -99.1% | 0.004 |

Syncs, counted by strace on Linux for one run of each build (all four
cases, 16,256 admissions with the warm-ups; both also made 12 `fsync`
calls): 16,256 `fdatasync` calls before, one per
admission, and 5,413 after. Of those, 256 are the warm-ups and 4,000 the
case of one first flight at a time, each admitted alone as before; the
12,000 admissions of the cases of 8, 64 and 256 at once took 1,157.

Compaction: with `--compaction` the bench also writes a log of 200,000 live
admissions (8 MB, most of the 240,000 the default budget admits in a
window), lets the relay open it, waits 61 s and then runs the case of 64 at
once, whose first admissions come due for the minute's compaction, which
rewrites the 8 MB. Before, the admission that came due held the lock through
the compaction, and every other waited for it and then for one sync each;
after, the writer compacts between batches and the admissions queued behind
it wait for it alone. 3 rounds each on C:, before against the final build,
which also repeat the four cases above:

| metric | before (n=3) | after (n=3) | change | p |
|---|---|---|---|---|
| c1 per_s | 1132.0 [1121.0..1150.0] | 1151.0 [1145.0..1161.0] | 1.7% | 0.127 |
| c1 p50_us | 861.1 [847.8..873.1] | 848.0 [841.5..854.0] | -1.5% | 0.275 |
| c1 p99_us | 1333.4 [1305.5..1340.7] | 1292.6 [1291.0..1303.8] | -3.1% | 0.050 |
| c1 max_us | 3767.7 [3404.3..4318.7] | 5776.8 [4606.3..5837.3] | 53.3% | 0.050 |
| c8 per_s | 1113.0 [1094.0..1131.0] | 4856.0 [4806.0..5050.0] | 336.3% | 0.050 |
| c8 p50_us | 7132.3 [7022.3..7194.3] | 1621.9 [1568.2..1631.2] | -77.3% | 0.050 |
| c8 p99_us | 8439.4 [8406.3..9833.3] | 2255.3 [2046.8..2341.1] | -73.3% | 0.050 |
| c8 max_us | 10776.5 [10571.8..11292.7] | 3798.2 [2964.2..5065.3] | -64.8% | 0.050 |
| c64 per_s | 1109.0 [1098.0..1126.0] | 37339.0 [34954.0..38580.0] | 3266.9% | 0.050 |
| c64 p50_us | 57970.8 [56883.6..58252.5] | 1686.5 [1622.0..1723.7] | -97.1% | 0.050 |
| c64 p99_us | 63543.0 [61605.5..64442.3] | 2267.7 [2151.0..5444.2] | -96.4% | 0.050 |
| c64 max_us | 65811.2 [64232.3..113340.8] | 2292.7 [2283.3..5484.1] | -96.5% | 0.050 |
| c256 per_s | 1105.0 [1103.0..1114.0] | 133665.0 [128135.0..135925.0] | 11996.4% | 0.050 |
| c256 p50_us | 232650.4 [229819.6..232727.4] | 1810.5 [1750.1..1879.3] | -99.2% | 0.050 |
| c256 p99_us | 246085.9 [238980.8..247178.3] | 2366.1 [2337.3..2418.2] | -99.0% | 0.050 |
| c256 max_us | 248468.8 [240295.2..472648.9] | 2599.8 [2507.5..2717.6] | -99.0% | 0.050 |
| compact200000 per_s | 1106.0 [1105.0..1108.0] | 33346.0 [30956.0..33972.0] | 2915.0% | 0.050 |
| compact200000 p50_us | 57531.6 [57470.2..57846.6] | 1582.7 [1527.5..1659.8] | -97.2% | 0.050 |
| compact200000 p99_us | 62851.3 [62768.1..64321.1] | 21631.5 [21264.7..25592.1] | -65.6% | 0.050 |
| compact200000 max_us | 79275.0 [78360.4..119452.8] | 21703.9 [21387.6..25756.4] | -72.6% | 0.050 |
| compact200000 failed | 1.000 [0.000..1.000] | 0.000 [0.000..0.000] | -100.0% | 0.190 |

The compaction shows as the after side's largest wait, 21.7 ms (21.4 to
25.8 ms); before, the largest was 79 ms (78 to 119 ms). One admission failed in two of the
three before runs and none after; the bench counts failures without their
cause. Both builds check the same Bloom filter, which at 200,000 live
fingerprints now and then finds a fresh one, and answers it `Replayed`.

What the tables say:

- What the item is for: with first flights arriving together, the relay
  admits as many as it can sync in one go. At 256 at once, 1,077 admissions
  a second become 136,023 on C:, 342 become 42,867 on D: and 200 become
  24,736 on the WSL volume, and the median wait falls from 236 ms, 749 ms
  and 1,281 ms to about two syncs: 1.8, 6.0 and 10.0 ms. At 8 at once it
  rises fourfold.
- What it costs: nothing measurable where admissions come one at a time:
  the `c1` rates and medians move less than 1% (p >= 0.45), and its largest
  waits both ways inside their spread. The hand-off to the blocking pool and
  back is now one to the writer thread and back. The relay starts one thread
  more.
- A burst of first flights no longer parks one blocking-pool thread each.
  Since the auth store's commands and compactions run on that pool
  ([open-items-auth.md](open-items-auth.md)), they no longer queue behind
  handshakes waiting for the log.

## The re-subscribe poll (kept, 3272faf7)

### What it cost

A subscribe whose registration falls silent passes it over and, when no
other can serve it, waits for a replacement until its recovery window ends:
2.5 s after it began, or 2.2 s after its last suspicion if that is later.
It asked the routing manager again every 50 ms meanwhile. Each ask was a
manager task (a lookup, a scan of the service's registrations, a formatted
reason) that logged a warning (`subscribe_no_usable_server_conn`, or
`subscribe_key_missing` once the silent registration was retired), and an
info line on the subscriber's side (`subscribe_waiting_for_replacement`).
A provider that falls silent leaves waiting as many subscribes as were
routed to it inside one acknowledgement budget (300 ms): up to a
namespace's burst, 200 at the default limits, and more across namespaces.
All of them were suspected at about the same moment, so their polls came
in bursts every 50 ms.

`resubscribe_bench` (new, pb-mapper-cli examples) runs a relay in its own
process, logging as the binary does by default (pretty, `info`, here to a
file), with one silent legacy registration per hundred subscribes. It
connects `n` subscribers (100, 400 or 1,000), sends their subscribes at
once, and measures from then until the last is refused: the manager's
fruitless asks (`asks`, each answer that found nothing to route to, counted
by a tracing layer), the lines the relay logged, the process's CPU time,
and the latency of a status request every 5 ms, which the manager accepts
and answers. Every subscribe waited and was refused in every Windows run
(`waited` = `refused` = `n`).

### The change

The routing manager keeps the subscribes waiting for a replacement by
service (`ReplacementWaiters`) and tells them to ask again (a
`SubcribeRetry` while they have no request outstanding) when what they would
find changes: a registration of the service comes, one is put back in
rotation (activity, an acknowledgement or a stream from one suspected), or
the service loses its last registration. A waiting subscribe asks again only
then, and is refused when its window ends. It is told once and must ask to
be told again, so a word to ask can never be taken for the answer to an ask.
Its connection's end removes its entry.

What a waiting subscribe observes:

- Served by a replacement that registers inside its window: as before, and
  at once rather than at the next 50 ms tick. The existing regression tests
  that wait for a replacement pass, and fail with no wake on registration.
- Served by a registration it did not pass over that is put back in
  rotation inside its window: as before. A new regression test
  (`a_waiting_subscribe_is_served_by_a_registration_that_recovers`), which
  passes on the poll, fails with no wake on that.
- Refused otherwise, with the same refusal (`service_not_available`,
  retryable, and the reason it was last told: the wake when the service
  loses its last registration keeps that "not registered", as the poll's
  last ask found it). It is refused at the end of its window; the poll's
  refusal came at the first tick at or after it, up to 50 ms later, and a
  registration arriving inside those 50 ms was taken.
- Its departure ends the wait at once (the poll noticed it at its next
  tick), and its place is held and freed as before.

### Measured

Before = 3272faf7's parent (b00c2156) plus the bench, after = 3272faf7.
Windows, 12 rounds each:

| metric | before (n=12) | after (n=12) | change | p |
|---|---|---|---|---|
| idle probe_p50_us | 1131.9 [974.3..1583.4] | 1382.5 [1031.7..1747.0] | 22.1% | 0.028 |
| idle probe_p99_us | 1820.6 [1454.1..2451.0] | 2196.3 [1644.8..3283.3] | 20.6% | 0.024 |
| idle probe_max_us | 2067.1 [1543.0..4021.9] | 2898.9 [1758.0..16808.7] | 40.2% | 0.038 |
| wait100 probe_p50_us | 1294.0 [1078.9..1761.1] | 1465.8 [973.5..1651.3] | 13.3% | 0.204 |
| wait100 probe_p99_us | 4415.4 [3665.7..5479.4] | 4710.9 [3686.5..10193.8] | 6.7% | 0.525 |
| wait100 probe_max_us | 15889.7 [9158.9..41085.5] | 24831.3 [7413.0..36279.1] | 56.3% | 0.248 |
| wait100 log_lines | 33873.5 [33577.0..33877.0] | 6270.0 [6049.0..6363.0] | -81.5% | 0.000 |
| wait100 asks | 3700.0 [3694.0..3700.0] | 200.0 [200.0..200.0] | -94.6% | 0.000 |
| wait400 probe_p50_us | 1508.4 [1296.1..2366.0] | 1291.0 [921.9..1909.5] | -14.4% | 0.033 |
| wait400 probe_p99_us | 12603.3 [7102.1..56883.6] | 14597.2 [8364.1..22302.5] | 15.8% | 0.094 |
| wait400 probe_max_us | 72042.1 [18864.7..158213.4] | 75841.5 [24960.9..183009.1] | 5.3% | 0.908 |
| wait400 log_lines | 130833.5 [130200.0..130959.0] | 20833.0 [20808.0..21150.0] | -84.1% | 0.000 |
| wait400 asks | 14800.0 [14709.0..14801.0] | 800.0 [800.0..800.0] | -94.6% | 0.000 |
| wait1000 probe_p50_us | 2007.2 [1384.7..2992.4] | 1317.1 [1033.6..1825.7] | -34.4% | 0.000 |
| wait1000 probe_p99_us | 74755.5 [45585.8..112553.7] | 32313.8 [19534.0..161498.8] | -56.8% | 0.038 |
| wait1000 probe_max_us | 166342.9 [70309.3..420084.4] | 121114.1 [67271.2..243444.3] | -27.2% | 0.273 |
| wait1000 log_lines | 324350.0 [318718.0..325277.0] | 50347.5 [50333.0..50623.0] | -84.5% | 0.000 |
| wait1000 asks | 36903.0 [36160.0..37000.0] | 2000.0 [2000.0..2000.0] | -94.6% | 0.000 |

WSL, 8 rounds each, for the CPU time, which Windows charges by the clock
tick: the `idle` case read 0 to 906 ms there for the same 2.5 s of probing.
In WSL some subscribes reached the relay after the first suspicion and were
refused at once, so `waited` at 1,000 ranged from 552 to 1,000 on both
sides (medians 918 and 876).

| metric | before (n=8) | after (n=8) | change | p |
|---|---|---|---|---|
| idle cpu_ms | 145.0 [120.0..190.0] | 145.0 [110.0..170.0] | 0.0% | 0.916 |
| idle probe_p50_us | 751.7 [666.7..868.0] | 734.6 [658.8..815.3] | -2.3% | 0.270 |
| idle probe_p99_us | 1162.3 [994.3..1256.9] | 1091.3 [989.1..1168.6] | -6.1% | 0.115 |
| wait100 cpu_ms | 340.0 [300.0..390.0] | 210.0 [160.0..270.0] | -38.2% | 0.001 |
| wait100 probe_p50_us | 792.3 [703.0..868.6] | 739.5 [670.0..809.2] | -6.7% | 0.294 |
| wait100 probe_p99_us | 1287.6 [1163.1..1482.9] | 1159.3 [1063.6..1365.1] | -10.0% | 0.012 |
| wait100 log_lines | 40760.0 [40603.0..40963.0] | 7725.0 [7611.0..7784.0] | -81.0% | 0.001 |
| wait100 asks | 4400.5 [4400.0..4418.0] | 200.0 [200.0..200.0] | -95.5% | 0.001 |
| wait400 cpu_ms | 1055.0 [870.0..1180.0] | 655.0 [600.0..820.0] | -37.9% | 0.001 |
| wait400 probe_p50_us | 795.4 [700.9..843.6] | 752.5 [671.3..829.7] | -5.4% | 0.529 |
| wait400 probe_p99_us | 1996.7 [1446.2..4046.0] | 1213.5 [1074.6..2389.3] | -39.2% | 0.027 |
| wait400 log_lines | 154949.5 [154679.0..155250.0] | 22450.5 [22339.0..22596.0] | -85.5% | 0.001 |
| wait400 asks | 17600.0 [17600.0..17608.0] | 800.0 [800.0..800.0] | -95.5% | 0.001 |
| wait1000 cpu_ms | 2010.0 [1350.0..2390.0] | 1160.0 [880.0..1530.0] | -42.3% | 0.002 |
| wait1000 probe_p50_us | 800.7 [708.3..862.5] | 757.5 [664.8..833.3] | -5.4% | 0.462 |
| wait1000 probe_p99_us | 3598.9 [2148.0..6803.6] | 6901.5 [2414.5..12572.9] | 91.8% | 0.021 |
| wait1000 log_lines | 355840.0 [221675.0..385919.0] | 50761.0 [44239.0..54309.0] | -85.7% | 0.001 |
| wait1000 asks | 40480.5 [24221.0..44007.0] | 1876.0 [1611.0..2000.0] | -95.4% | 0.001 |

What the tables say:

- Fruitless asks fall from 37 per waiting subscribe on Windows and 44 on
  Linux to 2: the first, and the one after the silent registration is
  retired. The relay's log for the wait falls by 81 to 86%.
- CPU time over the wait falls by 38% at 100 and 400 waiting and 42% at
  1,000 (Linux, whole process, the subscribers' side of the bench
  included), with the idle control unchanged.
- The probe's median does not move beyond noise at 100 and 400 on either
  system (Windows' idle control alone moved 22%); at 1,000 it falls 34% on
  Windows. Its 99th percentile is mixed: -57% at 1,000 on Windows, -39% at
  400 and +92% at 1,000 on Linux (3.6 to 6.9 ms). The waiting subscribes
  now ask in one burst when the silent registration is retired, where they
  polled in a burst every 50 ms; a probe that meets the one burst waits as
  long.

Verdict: kept. The manager's work and the relay's log for a wait fall by
an order of magnitude, CPU by about 40%, and a waiting subscribe takes a
replacement at once. It adds to the routing manager a registry of the
waiting subscribes, and a wake where a registration is added, put back in
rotation or removed.

## Buffer retention (kept, 2b6437b8)

### What holds memory

Every forwarding hop reads through one of two buffers, and each keeps its
largest size until its stream ends:

- `BufferReader`, which reads a plain stream: its length doubles while
  reads fill it, up to `MAX_BUF_SIZE`, 8 MiB, and falls back to 8 KiB after
  a read of less, but its capacity stays.
- `CommonBuffer` in `NormalMessageReader`, which reads a frame of known
  length, encrypted or a datagram: it is resized to each frame, so it keeps
  the largest frame's capacity, up to `MAX_MSG_LEN` (8 MiB and a tag).
  Frames are as large as the sender's reads, so they reach 8 MiB too.

A relayed tunnel has six such readers, one per direction on each of its
three hops (the consumer's end, the relay, the publisher's end), two of
them in the relay.

Options measured: a cap of 1 MiB on `MAX_BUF_SIZE` (`cap1m`: reads, and so
an encrypting forwarder's frames, of 1 MiB at most; frames up to 8 MiB from
an older peer are still read), and giving a buffer grown past 1 MiB back once
the length it needs is 8 KiB or less again (`shrink`). A 64 KiB cap was
measured before and rejected (README.md, "Workarounds measured and not
taken").

`buffer_retention_bench` (new, pb-mapper-cli examples) runs a relay and the
two ends of 8 tunnels in one process on the binary's allocator (mimalloc),
one carrier and codec setting per process. Each tunnel echoes 32 MiB through
one connection, written and read 64 KiB at a time, all 8 at once; the
connections then stay open and idle, and a second later the process's
memory is read: resident and committed private bytes (Windows'
`PROCESS_MEMORY_COUNTERS`). It also prints the burst's throughput. A first
version wrote and read each burst whole, from 64 MiB of buffers per tunnel,
which the allocator kept after the bench freed them: 618 to 1,032 MiB
resident afterwards whatever the option, which said nothing about the tunnels;
it now holds only 64 KiB per tunnel.

### Measured

Base = 3272faf7, each option = the same with only `buffer.rs` changed; 8
rounds each, every process idle at about 19 MiB resident (TCP) and 21 MiB
(QUIC) before its burst.

The 1 MiB cap:

| metric | base (n=8) | cap1m (n=8) | change | p |
|---|---|---|---|---|
| Tcp-codec=false idle_rss | 18.85 [18.60..19.10] | 18.80 [18.60..19.10] | -0.3% | 0.462 |
| Tcp-codec=false idle_private | 60.50 [59.90..62.80] | 60.55 [59.60..61.90] | 0.1% | 0.600 |
| Tcp-codec=false after_rss | 251.2 [229.7..266.4] | 114.2 [110.9..115.9] | -54.5% | 0.001 |
| Tcp-codec=false after_private | 320.5 [303.5..336.1] | 187.1 [181.9..190.8] | -41.6% | 0.001 |
| Tcp-codec=false burst_mib_s | 112.3 [85.50..185.7] | 125.1 [86.70..183.2] | 11.3% | 0.674 |
| Tcp-codec=true idle_rss | 18.95 [18.60..19.20] | 19.00 [18.90..19.20] | 0.3% | 0.637 |
| Tcp-codec=true idle_private | 60.55 [58.50..63.00] | 60.60 [59.90..61.70] | 0.1% | 0.958 |
| Tcp-codec=true after_rss | 214.7 [201.2..234.9] | 197.5 [182.1..206.2] | -8.0% | 0.002 |
| Tcp-codec=true after_private | 343.2 [333.4..369.0] | 332.2 [313.5..337.6] | -3.2% | 0.016 |
| Tcp-codec=true burst_mib_s | 104.8 [38.40..168.4] | 131.6 [105.3..171.0] | 25.6% | 0.093 |
| Quic-codec=false idle_rss | 20.90 [20.60..21.10] | 20.85 [20.70..21.10] | -0.2% | 0.875 |
| Quic-codec=false idle_private | 64.30 [61.80..65.40] | 63.55 [62.50..65.70] | -1.2% | 1.000 |
| Quic-codec=false after_rss | 146.4 [139.4..178.1] | 131.8 [124.6..147.3] | -10.0% | 0.006 |
| Quic-codec=false after_private | 330.4 [315.1..347.3] | 315.4 [297.6..322.7] | -4.5% | 0.009 |
| Quic-codec=false burst_mib_s | 53.20 [39.30..64.80] | 54.15 [38.80..60.90] | 1.8% | 0.834 |
| Quic-codec=true idle_rss | 20.70 [20.50..21.10] | 20.70 [20.40..20.90] | 0.0% | 0.753 |
| Quic-codec=true idle_private | 62.65 [61.10..65.20] | 62.50 [61.20..64.80] | -0.2% | 0.674 |
| Quic-codec=true after_rss | 226.2 [215.6..242.2] | 210.9 [194.0..214.4] | -6.7% | 0.001 |
| Quic-codec=true after_private | 381.4 [364.8..397.6] | 352.9 [331.5..371.2] | -7.5% | 0.002 |
| Quic-codec=true burst_mib_s | 60.30 [54.40..65.30] | 64.05 [49.40..66.90] | 6.2% | 0.462 |

Giving the buffer back:

| metric | base (n=8) | shrink (n=8) | change | p |
|---|---|---|---|---|
| Tcp-codec=false idle_rss | 18.90 [18.50..19.00] | 18.80 [18.60..19.20] | -0.5% | 0.462 |
| Tcp-codec=false idle_private | 60.60 [59.00..61.10] | 60.60 [59.10..62.40] | 0.0% | 0.529 |
| Tcp-codec=false after_rss | 258.3 [224.1..266.8] | 287.1 [274.8..314.3] | 11.2% | 0.001 |
| Tcp-codec=false after_private | 328.7 [295.0..337.0] | 359.6 [347.9..388.0] | 9.4% | 0.001 |
| Tcp-codec=false burst_mib_s | 122.7 [82.50..147.7] | 110.2 [99.60..125.4] | -10.2% | 0.401 |
| Tcp-codec=true idle_rss | 19.10 [18.90..19.30] | 19.05 [18.70..19.40] | -0.3% | 0.834 |
| Tcp-codec=true idle_private | 61.20 [60.00..62.70] | 61.05 [59.20..62.70] | -0.2% | 0.637 |
| Tcp-codec=true after_rss | 215.8 [196.5..240.1] | 231.3 [214.0..256.1] | 7.2% | 0.041 |
| Tcp-codec=true after_private | 348.8 [341.9..372.5] | 359.8 [342.5..405.6] | 3.2% | 0.227 |
| Tcp-codec=true burst_mib_s | 111.0 [75.90..153.7] | 118.2 [87.00..168.7] | 6.5% | 0.294 |
| Quic-codec=false idle_rss | 20.60 [20.50..20.80] | 20.65 [20.40..21.20] | 0.2% | 0.875 |
| Quic-codec=false idle_private | 62.45 [61.40..63.60] | 62.30 [61.10..64.80] | -0.2% | 0.674 |
| Quic-codec=false after_rss | 148.2 [138.9..166.6] | 154.9 [136.9..185.0] | 4.6% | 0.529 |
| Quic-codec=false after_private | 330.4 [323.0..356.4] | 330.6 [324.6..350.0] | 0.1% | 1.000 |
| Quic-codec=false burst_mib_s | 59.40 [48.90..62.00] | 58.00 [48.50..63.10] | -2.4% | 0.637 |
| Quic-codec=true idle_rss | 20.60 [20.50..20.80] | 20.80 [20.60..21.10] | 1.0% | 0.018 |
| Quic-codec=true idle_private | 62.15 [61.60..62.50] | 62.40 [61.60..65.40] | 0.4% | 0.564 |
| Quic-codec=true after_rss | 223.3 [215.3..263.7] | 222.8 [205.6..260.3] | -0.2% | 0.674 |
| Quic-codec=true after_private | 378.0 [363.9..410.5] | 372.1 [356.0..449.7] | -1.5% | 0.753 |
| Quic-codec=true burst_mib_s | 62.95 [53.80..68.20] | 64.20 [30.30..66.20] | 2.0% | 0.529 |

Giving the buffer back held no less: 11% more resident on TCP without the
codec (p = 0.001) and 7% more with it (p = 0.04), nothing on QUIC. A buffer
is given back only when a read or a frame of 8 KiB or less follows a large
one, which these tunnels, idle after the burst, did not see; and where it
is, the next large read reallocates. Dropped.

The cap lowers what the idle tunnels hold in all four cases: resident memory
by 55% on TCP without the codec (251 to 114 MiB), where the readers' buffers
were most of it, and by 7 to 10% with the codec and on QUIC. There, most of
what stays is not the readers': with the cap a tunnel's six readers hold
6 MiB at most, while the process holds 12 to 24 MiB a tunnel more than
before the burst.

`relay_transport_bench`, base against the cap, 30 rounds each:

| metric | base (n=30) | cap1m (n=30) | change | p |
|---|---|---|---|---|
| Tcp codec=false MiB/s | 52.30 [36.90..74.30] | 56.90 [33.60..77.30] | 8.8% | 0.240 |
| Tcp codec=false p50 us | 165.7 [147.3..300.0] | 160.2 [136.3..278.8] | -3.3% | 0.882 |
| Tcp codec=false p95 us | 277.6 [231.3..439.0] | 292.2 [219.3..435.9] | 5.3% | 0.515 |
| Tcp codec=false p99 us | 408.3 [320.4..699.8] | 415.9 [311.6..1038.0] | 1.9% | 0.739 |
| Tcp codec=true MiB/s | 49.85 [9.600..71.50] | 51.00 [36.10..71.10] | 2.3% | 0.246 |
| Tcp codec=true p50 us | 192.6 [171.3..291.2] | 199.7 [175.8..316.7] | 3.7% | 0.084 |
| Tcp codec=true p95 us | 317.9 [241.2..412.4] | 323.2 [253.2..540.3] | 1.7% | 0.383 |
| Tcp codec=true p99 us | 419.4 [327.8..603.5] | 428.3 [373.1..843.4] | 2.1% | 0.408 |
| Quic codec=false MiB/s | 69.00 [51.80..80.10] | 71.55 [59.20..81.90] | 3.7% | 0.065 |
| Quic codec=false p50 us | 225.8 [204.1..353.1] | 225.6 [199.0..395.5] | -0.0% | 0.982 |
| Quic codec=false p95 us | 380.8 [321.8..570.8] | 382.9 [295.5..563.4] | 0.6% | 0.554 |
| Quic codec=false p99 us | 513.9 [388.5..15296.2] | 543.6 [417.2..13839.1] | 5.8% | 0.554 |
| Quic codec=true MiB/s | 62.55 [48.90..71.80] | 64.00 [47.60..75.00] | 2.3% | 0.099 |
| Quic codec=true p50 us | 227.1 [195.6..367.8] | 236.8 [199.8..351.0] | 4.3% | 0.209 |
| Quic codec=true p95 us | 393.3 [306.4..541.4] | 425.0 [336.5..577.2] | 8.1% | 0.066 |
| Quic codec=true p99 us | 531.7 [396.7..18203.8] | 543.5 [428.2..10700.3] | 2.2% | 0.690 |

No metric is slower beyond the noise. The largest moves are TCP throughput
without the codec, +8.8% (p = 0.24), and the encrypted QUIC round trip's
p95, +8.1% (p = 0.07). One base run stalled once, in encrypted TCP, and no
run with the cap; without that run the medians are 50.50 and 51.00 MiB/s.
A 64 KiB cap cost QUIC 16.9% and 7.7% (README.md); 1 MiB costs nothing
measurable.

Verdict: kept. Memory falls in every case and throughput does not. Frames
from an older peer, up to 8 MiB, are still read, and a reader of such
frames still keeps 8 MiB; between peers of this version every reader stays
at 1 MiB.

## Files

- `open-items-relay-handshake_bench.txt`: replay admission, on C:
  (`hs-c-windows-C`), D: (`hs-d-windows-D`) and WSL's ext4
  (`hs-wsl-linux-ext4`), the compaction rounds on C: (`hs-compact-windows-C`),
  and strace's sync counts.
- `open-items-relay-resubscribe_bench.txt`: the re-subscribe wake, Windows
  (`rs3-windows`) and Linux (`rs-wsl-linux`). Two earlier Windows sessions are
  left out: one of a version of the bench that sent the subscribes as each
  connected, so that some arrived after the first suspicion and never
  waited, and one of a version of the change that woke the waiting
  subscribes a second time when the retired registration's connection ended.
- `open-items-relay-buffer_retention_bench.txt`: memory after a burst, base
  against each option (`br-cap`, `br-shrink`).
- `open-items-relay-relay_transport_bench.txt`: base against the cap, 30
  rounds each (`rt-cap`).
