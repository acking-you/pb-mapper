# pb-mapper (Node)

TypeScript/JavaScript bindings for [pb-mapper](https://github.com/acking-you/pb-mapper),
built with Node-API (napi-rs). Publish a private TCP/UDP service with `register`
and reach it on a local port with `connect`: through the relay by default, or
end to end when a connection opts into direct and the network allows it.

```bash
npm install pb-mapper
```

The package ships prebuilt native addons for 64-bit glibc Linux, macOS, and
Windows. It supports Node.js 18+ and Bun 1.1+. Browsers and edge runtimes cannot
load Node-API addons.

```ts
import { Client } from "pb-mapper";

const client = new Client({
  server: "relay.example.com:7666",
  credential: process.env.MSG_HEADER_KEY!,
  keepAlive: true,
});

const reg = await client.register({
  key: "echo",
  localAddr: "127.0.0.1:8080",
  transport: "tcp",
});
await reg.waitReady();

const admin = client.admin();
const issued = await admin.issueKey(3600, "agent");
await admin.revokeKey(issued.keyId);

await reg.stop();
```

Build the native addon from a source checkout:

```bash
bun install
bun run build:release   # LTO + strip
bun test                # smoke + e2e (e2e needs `cargo build --bin pb-mapper`)
```

### TCP idle connection reclamation

Use `client.registerWithIdleTimeout(request, idleTimeoutMs)` to reclaim abandoned
data connections on the publishing device. The timeout is a non-negative safe
integer in milliseconds; zero disables it. Traffic in either direction resets
the deadline. UDP is rejected. `register(request)` keeps the existing default,
and other registrations and control connections are unaffected.

Choose a deadline longer than the service's legitimate quiet periods, or use
application heartbeats. This is a publisher SDK policy, not a relay protocol
change: existing compatible relays and subscribers do not need an upgrade.

### Recovery diagnostics

`registration.diagnostics()` and `connection.diagnostics()` report the running
SDK version, latest attempt phase, failure reason, timing and shared setup counts.
`client.notifyNetworkChange()` wakes pending recovery after a host network/resume
notification; healthy forwarding stays open. Workers retry initial DNS failures,
refresh relay addresses through the OS resolver, and share bounded setup capacity.
See [recovery design](../docs/weak-network-recovery.md) for bounds and limitations.

### Direct connections with relay fallback

Direct is opt-in on every side: the relay enables its UDP service, the
publisher passes `allowDirect: true`, and each connect chooses `path: "auto"`.
`"relay-only"` is the default, so existing calls keep the relay protocol.

```ts
const reg = await client.register({
  key: "echo", localAddr: "127.0.0.1:8080", transport: "tcp", allowDirect: true,
});
const conn = await client.connect({
  key: "echo", localAddr: "127.0.0.1:9090", transport: "tcp", path: "auto",
});
await conn.waitReady();
console.log(conn.diagnostics().direct); // populated after path selection
console.log(JSON.parse(await client.sessionsJson()));
await conn.stop();
await reg.stop();
```

`auto` tries UDP hole punching and mutually authenticated QUIC, then falls back
to the relay if setup fails before commit. After commit a failed connection
closes and is never replayed onto the relay. Established direct connections
survive coordinator loss and credential revocation; local `stop()` closes them.
Session reports carry ages and `stale`, `conflicting` and `restored` flags; they
never authorize a connection.
