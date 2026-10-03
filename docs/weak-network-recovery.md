# Weak-network recovery

The register, connect, and relay roles must recover when a route returns but an
old TCP socket remains unusable. A local application still running, a successful
TCP connect, and an entry in the relay inventory are different observations;
none alone proves that the original bidirectional control socket works.

## Confirmed failure mechanisms

The fault tests reproduce these problems in the previous implementation:

| Fault | Previous behavior | Changed behavior |
| --- | --- | --- |
| Cancel a fragmented control-frame read | Legacy checksum / V2 framing errors; consumed bytes were lost | Header/body progress stays in the reader across cancellation; counters advance only after authentication |
| An old registration socket blackholes replies while new sockets work | Waits for the 30-second response timeout | One deadline covers TCP dial, write, and authenticated register response |
| A health RPC stalls | The listener's select branch awaits it, blocking new accepts and cancellation | One owned probe runs concurrently with accepts and forwarding |
| A stream ACK takes longer than 300 ms | Immediately unregisters an otherwise live control socket | Tries another candidate, marks the old one suspect, and confirms silence before retiring it |
| Old control replies disappear, but status RPCs still list the connection | A `Present` probe can keep the broken socket indefinitely | The control receive deadline is authoritative even if inventory says present |

The framing fix follows Tokio's [cancellation-safety contract for
`select!`](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety):
`read` preserves progress when cancelled, whereas `read_exact` and integer reads
must not hold the only copy of partial-frame state in a discarded future.

## Lifetimes and latency budgets

1. **A local listener belongs to its configured endpoint.** It binds before
   readiness is reported and remains bound during remote outages. A successful
   status probe or actual subscription establishes readiness. The configured
   failure threshold changes reported status to `retrying`; it does not close
   the listener. A permanent authorization refusal still stops the worker.
2. **A setup attempt belongs to a deadline.** Registration starts with a two-second
   total budget. Successful setup latency updates a smoothed estimate and its
   variation; the next budget is `SRTT + 4 * variation`, bounded to 1–5 seconds.
   A timeout increases the budget. A smaller `PB_MAPPER_CONTROL_IO_TIMEOUT` still
   applies. This application-level estimator borrows the smoothing from
   [RFC 6298](https://www.rfc-editor.org/rfc/rfc6298.html); it does not change TCP's
   retransmission timer. Transport retry sleeps grow from 100 ms to 2 seconds,
   with 75–100% jitter. Explicit relay rejections keep their separate 5–80 second
   backoff and permanent refusals remain terminal.
3. **A control socket must receive authenticated traffic.** With the defaults,
   suspicion begins at six seconds; eight seconds without a complete inbound
   control frame closes that socket. A separate status lookup can establish that
   registration was lost sooner, but cannot postpone this deadline. The watchdog
   has its own timer, so heartbeat scheduling does not add another interval.
4. **One slow stream is insufficient evidence to unregister a service.** The
   relay excludes that candidate for the current subscribe and gives the control
   socket a confirmation grace of at least two seconds. Late ACKs and heartbeats
   restore its health. Only continued silence retires it. Confirmation carries
   both connection ID and generation, so a delayed timer cannot remove a replacement.
   Each connection's ACK budget starts at the configured 300 ms floor, doubles
   after a timeout, and incorporates measured request-to-ACK latency with a margin
   after success. The normal adaptive ceiling is five seconds; an explicitly
   larger ACK floor is retained. Data-ready waiting also follows that budget.
5. **A subscription has a five-second total setup deadline.** The client may retry
   transient failures within that window, using fresh TCP and authenticated
   sessions. The relay's whole selection/setup loop is also bounded to five
   seconds, even if new failing candidates continually register. No application
   byte is read before subscribe succeeds, so these retries do not replay HTTP,
   SSH, or other application payloads. Permanent structured refusals are not retried.
6. **Forwarding belongs to the tunnel, not its control socket.** Replacing a
   control connection leaves healthy data streams alive. Explicit tunnel stop
   cancels all owned streams, probes, and control writer tasks. A data socket that
   actually resets cannot be resumed as an arbitrary byte stream; its application
   must reconnect. No automatic replay of already forwarded bytes is attempted.

## Resource bounds

- At most one health RPC per connect listener and one registration probe per
  register worker. Stream failure notifications coalesce in a one-element queue.
- At most 64 concurrent setups per connect listener and per register worker.
  Completed setups release their slots before forwarding, so this is not a
  64-session limit. Excess local connects wait in the OS listener backlog;
  register workers can decline excess setup work so the relay tries another worker.
- Control writer queues hold at most 64 messages. Saturation fails the control
  path rather than growing an unbounded queue. Register pool size, namespace
  stream quotas, and admission limits continue to apply.
- Relay routing uses nonblocking mailbox sends. One stalled socket cannot block
  the shared manager, status queries, or retirement timers. A saturated control
  mailbox is removed from routing so another candidate can serve the request.
- Relay TCP dials race at most two address candidates, starting the second after
  250 ms and bounding each multi-address attempt to two seconds inside the
  existing overall setup deadline. Losing/abandoned attempts are cancelled.
- An admitted subscription reuses its namespace slot and rate token during
  failover; recovery cannot reject itself merely because the namespace is full.
- At most one confirmation timer per relay control connection. Owned task sets
  reap completed work; dropping a worker also aborts its writer and probes.
- The new frame reader retains bounded progress and one existing payload buffer;
  authenticated payload limits and pre-authentication allocation checks remain.
- There is no additional periodic healthy-path network probe. Existing heartbeat
  and status intervals remain unchanged; unhealthy retries use the bounded backoff.

## Verification and limits

Run `cargo test -p pb-mapper-protocol fragmented_frame` and
`cargo test -p pb-mapper-cli --test network_recovery -- --nocapture`.
The integration tests use isolated loopback relays, encrypted traffic, and an owned
fault proxy. Only the health interval is shortened to 200 ms so a background probe
can be reached quickly; register heartbeat/tolerance/grace retain production defaults.

Representative development measurements on Linux/WSL2:

| Scenario | Before | After |
| --- | --- | --- |
| Route restored; previous registration handshake remains blackholed | 30.13 s until registered | About 2.09 s; then real encrypted echo succeeds |
| New request while a status probe is stalled | Still blocked at 750 ms; probe occupied the loop for 5 s | 9–19 ms in the recorded runs |
| 440 ms injected RTT | Connection reset and registration churn | Three encrypted streams succeed without changing control IDs/generations |
| Three one-second jitter episodes | Covered by the reproduced ACK/framing failures above | Existing and new streams complete in about 1.6–2.7 s per episode; control registrations survive |
| Return path of established controls blackholed; inventory still healthy | No replacement within 11 s | About 8.2 s, while an existing data stream keeps working |
| Three services actually leave one relay; all publisher workers hold blackholed attempts when the route returns | Not all services/payloads recover within 8 s | Two outage cycles recover all three registrations and encrypted payloads in 1.21 s / 1.18 s; another host's stream and control generations stay intact |
| Ten seconds of handshake blackholing | Unbounded OS dial / 30 s response waiting | Six attempts across two workers; stop under 1 ms in the recorded run |
| 96 local callers during an outage | No setup concurrency bound in the listener | 64 setup sockets plus one probe |

These are fault-injection observations, not Internet latency guarantees or a
native Windows/macOS qualification. While the physical route is absent, packets
cannot be delivered. After a sustained outage, a remaining setup budget plus the
transport backoff is normally bounded by roughly seven seconds with defaults,
provided the relay accepts the new registration. Authentication refusals, quotas,
relay/storage overload, and real DNS/address changes have different recovery
conditions. Endpoint DNS is still resolved at worker startup.

The protocol and credential formats are unchanged. Deploy the updated relay to
get delayed-retirement/ACK-budget behavior; rebuild register/connect processes
and SDK consumers to get framing, watchdog, listener, and setup fixes. Updating a
standalone CLI does not update a pb-mapper SDK embedded in another application.

The multi-service regression waits for the three affected keys to disappear from
relay inventory and for the publisher pool to hold outage attempts before
restoring connectivity. It checks fresh payloads through the existing connect
handles after recovery, rather than treating an alive process or a cached ready
flag as success. This reproduces a recovery defect; it does not establish the
cause or duration of an unobserved production outage.

## Code map

- `crates/pb-mapper-protocol/src/frame_read.rs`: persistent frame progress.
- `crates/pb-mapper-protocol/src/secure/frame.rs`: V2 framing/authentication.
- `crates/pb-mapper-client/src/recovery.rs`: setup latency estimation and jitter.
- `crates/pb-mapper-client/src/client/mod.rs`: independent listener/probe lifecycle.
- `crates/pb-mapper-client/src/client/stream.rs`: bounded, pre-payload setup retries.
- `crates/pb-mapper-client/src/server/mod.rs`: register deadlines and control watchdog.
- `crates/pb-mapper-server/src/client.rs`: candidate fallback and whole-setup deadline.
- `crates/pb-mapper-server/src/runtime.rs`: suspicion confirmation and ACK budgets.
- `crates/pb-mapper-cli/tests/network_recovery.rs`: fault proxy and end-to-end cases.

## Additional SDK review for 0.5.1

- Concurrent `stop()` calls wait for the same worker cleanup. Cancelling a stop
  future leaves the worker owned by the handle, so dropping the handle still
  aborts it instead of detaching it.
- Administrator writes become non-retryable before the first write is polled.
  A timeout after a partial write is ambiguous and cannot safely replay a
  credential issuance or root-key rotation. Explicit replay-salt rejection is
  still eligible for one fresh-session retry.
- Raw `Credential` and `ClientConfig` debug output redact key bytes and unparsed
  credentials, including nested registration tracing fields.
- The release workflow publishes all seven public Rust crates in dependency
  order, including `pb-mapper-server` and `pb-mapper-cli`.

Focused regressions in `pb-mapper-client` cover blackholed address candidates,
bounded candidate concurrency and cancellation, concurrent/cancelled SDK stop,
partial administrator writes, and debug credential redaction. The relay
regression runs with one namespace stream slot and one rate token; its first
control deliberately withholds ACK while a second control supplies the stream.
It fails before the admission-accounting fix and passes afterward.

## Recovery hardening in 0.5.2

- The manager mailbox uses cancellation-safe admission: a pending send has not
  delivered a command. Each socket's commands carry a lifetime token; after its
  first cleanup, duplicate cleanup and late activity cannot touch a reused ID.
  Accepted administrator mutations report their actual outcome. Dropping a relay
  aborts its owned listener, sweep, shutdown and status tasks.
- SDK and CLI workers retain the configured relay hostname. Initial DNS failure
  remains retryable. They coalesce refreshes, refresh on network/transport failure,
  cache for 60 seconds, and retain last-known candidates if lookup fails. Refresh
  attempts are at least two seconds apart; routine TTL refresh runs in the
  background. The OS resolver remains authoritative for hosts, VPN and TUN DNS.
  Each endpoint owns at most one lookup; eight process-wide permits bound actual
  OS lookups, including ones whose caller was cancelled. A blocking OS lookup
  cannot be forcibly cancelled and keeps its permit until completion.
- A shared watcher observes interface address changes (native notifications on
  Linux/Windows; a single two-second poller elsewhere). Host integrations can also
  call `Client::notify_network_change()` / Node `notifyNetworkChange()`. Notifications
  are hints, never proof of connectivity, and bursts are coalesced. The initial
  address inventory does not count as a change. WSL may not expose a physical
  Wi-Fi interruption as an interface event; existing protocol deadlines remain
  the fallback. Authenticated recovery on the same configured relay wakes workers
  waiting to retry, without cancelling another worker's healthy handshake.
- Admission is shared by all mappings to one configured relay address in a
  process: at most eight control setups and 64 data setups. Successful forwarding
  releases its setup permit. Existing CLI processes have separate budgets; these
  are not machine-wide limits. Public status RPCs have a five-second maximum
  including admission, preventing them from consuming recovery capacity for 30 seconds.
- Only actual timeouts expand the setup deadline. A relay that answers "service
  unavailable" supplies a latency sample and remains distinguishable from DNS,
  transport, timeout, rejection and network-change failures. Queue wait is not
  measured as relay latency. Repeated recovery logs are sampled at powers of two.
- Rust and Node handles expose `diagnostics()`: the **running SDK** version,
  latest attempt phase, attempt/failure counters, authenticated reply age, setup
  latency, retry/DNS ages and shared setup use. No credentials or business payloads
  are included. Pool diagnostics describe the latest worker attempt; aggregate
  readiness remains the handle's `status()`. CLI workers emit a snapshot every
  30 seconds. Healthy control/data sockets survive network hints.

Regression coverage includes a pending-send/cancel cleanup race, a completed
mutation concurrent with revocation, parent relay cancellation, stale commands
following ID reuse, failed/changed DNS and cancelled waiters, a simulated
130-second outage, delayed ACKs, repeated jitter, 20 mappings sharing admission,
and encrypted end-to-end recovery after a stalled handshake. Fault injection is
isolated from production interfaces. These changes shorten software recovery
once connectivity returns; they cannot carry traffic through an unavailable Wi-Fi
link or resume arbitrary application bytes after their data socket is lost.
