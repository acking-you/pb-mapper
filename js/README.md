# pb-mapper (Node)

TypeScript/JavaScript bindings for [pb-mapper](https://github.com/acking-you/pb-mapper),
built with Node-API (napi-rs). Publish a private TCP/UDP service with `register`
and reach it on a local port with `connect`. Every connection goes peer to peer
over QUIC when the networks allow it and falls back to the relay before any
byte is sent when they do not. Reconnection and recovery are automatic.

```bash
npm install pb-mapper
```

The package ships prebuilt native addons for 64-bit glibc Linux (x64, arm64),
macOS (x64, arm64) and Windows x64. It supports Node.js 18+ and Bun 1.1+.
Browsers and edge runtimes cannot load Node-API addons.

```ts
import { Client } from "pb-mapper";

const client = new Client({
  server: "relay.example.com:7666",
  credential: process.env.MSG_HEADER_KEY!,
  keepAlive: true,
});

// Publish a local service...
const reg = await client.register({
  key: "web", localAddr: "127.0.0.1:8080", transport: "tcp",
});
await reg.waitReady();

// ...and reach it on a local port, direct first.
const conn = await client.connect({
  key: "web", localAddr: "127.0.0.1:9090", transport: "tcp",
});
await conn.waitReady();

await conn.stop();
await reg.stop();
```

## Paths

The relay carrier remains TCP by default. Set `relayTransport: "quic"` in
`new Client(...)` to use QUIC for registration, connection and SDK requests;
`client.relayTransport()` reads it back. The relay must enable
`--quic-listen IP:UDPPORT`; authenticated discovery still needs its TCP port,
and `await client.relayQuicPort()` asks over it whether QUIC is offered (`null`
when not). `await client.relayDiagnostics()` exposes pooled connection reuse,
RTT, loss and byte counters; tunnel `diagnostics().relayTransport` shows the
selected carrier. This setting is independent of the direct/relay policy.

`register` allows direct sessions and `connect` uses `path: "auto"` by
default: try UDP hole punching and mutually authenticated QUIC, and fall back
to the relay if setup fails before commit. After commit a failed connection
closes and is never replayed onto the relay. After a fallback, the tunnel sends
its next connections straight to the relay for a while (10 s doubling to 5 min,
cleared by a direct success or `notifyNetworkChange()`).

To stay on the relay, register with `allowDirect: false` or connect with
`path: "relay-only"`. Established direct connections survive coordinator loss
and credential revocation; local `stop()` closes them.

## Listening for changes

Automatic gateway mapping is off by default. Set `portMapping: true` in
`new Client({ server, credential, portMapping: true })` to request a UDP mapping
when a direct endpoint is created. Both register and connect use this setting;
relay-only tunnels leave the router alone. No extra build flag is needed for
the Node package. `diagnostics().portMapping` shows current state and counters.

```ts
const events = conn.subscribeDiagnostics();
try {
  for (;;) {
    const event = await events.next(); // null after tunnel cleanup
    if (event === null) break;
    console.log(event);
  }
} finally {
  events.close(); // also cancels pending next() calls
}
```

Each subscription replays recent history and then receives each transition,
without polling or folding live events. Its queue holds 128 events. A slow
reader gets a rejection naming the skipped count; refresh `diagnostics()` and
continue reading. `next(timeoutMs)` rejects on timeout without consuming an
event. Status and diagnostics are independent subscriptions.

Mapping events include discovery, gateway/protocol selection, requests,
activation, renewal (even if the address is unchanged), expiry, retries,
network changes, cancellation and cleanup. `event.mapping` contains the
generation, local port, gateway, protocol, external address, lifetime,
active flag and bounded error. A failed renewal can leave `active: true`
until expiry. `release_sent` is an unacknowledged PCP/NAT-PMP deletion;
`released` is a confirmed UPnP deletion; `release_failed` is not success.

```ts
// Resolves on every status change; once the tunnel settles as "stopped" or
// "failed:<reason>", every call resolves at once, which ends the loop.
let status = conn.status();
while (status !== "stopped" && !status.startsWith("failed")) {
  status = await conn.nextStatus();
  console.log("status:", status);
}
```

`diagnostics()` is a credential-free snapshot for dashboards and logs:

- `events`: the last 16 transitions, including `phase_changed`,
  `direct_stage_changed`, `session_started`, `session_closed`, `port_mapping`,
  `relay_ready`, `relay_failed`, `network_changed`, `direct_established`,
  `direct_fallback` and `stopped`, each with a `seq` so a poller
  can print only new ones, plus `latencyMs`, `failure`, `retryInMs`,
  `setupMs` or `stage` as they apply;
- `direct`: sessions per path, active sessions, fallbacks, and the attempt in
  flight with its stage (`negotiating`, `discovering`, `punching`,
  `committing`);
- recovery counters: attempts, consecutive failures, setup latency, next retry.

Call `client.notifyNetworkChange()` from the host's network-change or resume
notification: waiting retries start at once and direct is tried again.

## Credentials and the relay

With the administrator key, `client.admin()` manages temporary `pbmt1_`
credentials and inspects the relay:

```ts
const admin = client.admin();
const issued = await admin.issueKey(3600, "agent-session");  // ttl in seconds
await admin.renewKey(issued.keyId, 7200);
console.log(await admin.listKeys(), await admin.listServices(), await admin.listConnections());
await admin.revokeKey(issued.keyId);                           // live relay connections close
```

Every credential has its own namespace: `client.listKeys()`, `serviceStatus()`,
`sessionsJson()` and `remoteId()` only ever see that namespace.

## Examples

Runnable scripts in [`examples/`](examples) read `PB_MAPPER_SERVER` and
`MSG_HEADER_KEY`:

| Script | Shows |
| --- | --- |
| [`register.mjs`](examples/register.mjs) | Publish a TCP or UDP service, optionally relay-only |
| [`connect.mjs`](examples/connect.mjs) | Reach it, listen with `nextStatus()`, print new events |
| [`admin.mjs`](examples/admin.mjs) | The credential lifecycle and the relay inventory |
| [`remote-control.mjs`](examples/remote-control.mjs) | One scoped credential per agent session, revoked at the end |

## TCP idle connection reclamation

`client.registerWithIdleTimeout(request, idleTimeoutMs)` reclaims abandoned
data connections on the publishing device. The timeout is a non-negative safe
integer in milliseconds; zero disables it. Traffic in either direction resets
the deadline. UDP is rejected. Choose a deadline longer than the service's
legitimate quiet periods, or use application heartbeats.

## Building from source

```bash
bun install
bun run build:release   # LTO + strip
bun test                # smoke + e2e (e2e needs `cargo build --bin pb-mapper`)
```
