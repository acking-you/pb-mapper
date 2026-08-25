# pb-mapper (Node)

TypeScript/JavaScript bindings for a deployed pb-mapper relay, built with Node-API (napi-rs).

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
bun run build:release   # LTO + strip; linux-x64 is ~2 MB
bun test                # smoke + e2e (e2e needs `cargo build --bin pb-mapper`)
```
