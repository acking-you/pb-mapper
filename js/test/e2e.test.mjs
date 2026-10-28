import { expect, test } from "bun:test";
import { Client } from "../index.js";
import manifest from "../package.json";
import {
  ADMIN_KEY,
  echoRoundTrip,
  findPbMapperBinary,
  reserveListenAddress,
  startEchoServer,
  startRelay,
} from "./helpers/relay.mjs";

const READY_MS = 10_000;
const inCi = Boolean(process.env.CI || process.env.GITHUB_ACTIONS);
const hasRelay = Boolean(findPbMapperBinary());

test("CI builds the pb-mapper binary before N-API e2e", () => {
  if (inCi) {
    expect(findPbMapperBinary()).toBeTruthy();
  }
});

async function withRelay(run) {
  const relay = await startRelay();
  expect(relay).toBeTruthy();
  try {
    return await run(relay);
  } finally {
    await relay.stop();
  }
}

function clientFor(relay, credential = relay.adminKey) {
  return new Client({
    server: relay.address,
    credential,
  });
}

test.skipIf(!hasRelay)("napi mapping opt-in and independent diagnostic subscriptions stop cleanly", async () => {
  await withRelay(async (relay) => {
    const client = new Client({ server: relay.address, credential: relay.adminKey, portMapping: true });
    const echo = await startEchoServer();
    const registration = await client.register({ key: "mapping-events", localAddr: echo.address, transport: "tcp", allowDirect: false });
    const first = registration.subscribeDiagnostics();
    const second = registration.subscribeDiagnostics();
    try {
      await registration.waitReady(READY_MS);
      expect(registration.diagnostics().portMapping.current.stage).toBe("relay_only");
      const a = await first.next(READY_MS);
      const b = await second.next(READY_MS);
      expect(a.seq).toBe(b.seq);
      expect(a.mapping.stage).toBe("relay_only");
      first.close();
      expect(await first.next(READY_MS)).toBeNull();
      await registration.stop();
      const seen = [];
      for (let event; (event = await second.next(READY_MS));) seen.push(event);
      expect(seen.at(-1).kind).toBe("stopped");
      expect(seen.some(e => e.kind === "relay_ready")).toBe(true);
      expect(JSON.stringify(seen)).not.toContain(relay.adminKey);
    } finally {
      first.close(); second.close();
      await registration.stop(); await echo.close();
    }
  });
}, 30_000);

test.skipIf(!hasRelay)(
  "napi QUIC relay multiplexes streams and exposes live diagnostics",
  async () => {
    const relay = await startRelay({ quic: true });
    const echo = await startEchoServer();
    const client = new Client({
      server: relay.address,
      credential: relay.adminKey,
      relayTransport: "quic",
    });
    let registration;
    let connection;
    try {
      registration = await client.register({
        key: "quic-echo", localAddr: echo.address, transport: "tcp", codec: true,
        allowDirect: false,
      });
      await registration.waitReady(READY_MS);
      const listenAddr = await reserveListenAddress();
      connection = await client.connect({
        key: "quic-echo", localAddr: listenAddr, transport: "tcp", path: "relay-only",
      });
      await connection.waitReady(READY_MS);
      await Promise.all([1, 2, 3].map(async (n) => {
        const payload = Buffer.alloc(64 * 1024, n);
        expect((await echoRoundTrip(listenAddr, payload)).equals(payload)).toBe(true);
      }));
      expect(connection.diagnostics().relayTransport).toBe("quic");
      expect(client.relayTransport()).toBe("quic");
      expect(await client.relayQuicPort()).toBeGreaterThan(0);
      const diagnostics = await client.relayDiagnostics();
      expect(diagnostics.transport).toBe("quic");
      expect(diagnostics.connectionsCreated).toBe(1);
      expect(diagnostics.connections[0].pooled).toBe(true);
      expect(diagnostics.streamsReused).toBeGreaterThan(0);
      expect(diagnostics.connections[0].sentBytes).toBeGreaterThan(0);
      expect(diagnostics.connections[0].receivedBytes).toBeGreaterThan(0);
      expect(JSON.stringify(diagnostics)).not.toContain(relay.adminKey);
    } finally {
      await connection?.stop();
      await registration?.stop();
      await echo.close();
      await relay.stop();
    }
  },
  30_000,
);

test.skipIf(!hasRelay)(
  "napi idle policy preserves registration lifecycle with a zero deadline",
  async () => {
    await withRelay(async (relay) => {
      const client = clientFor(relay);
      const echo = await startEchoServer();
      const registration = await client.registerWithIdleTimeout({
        key: "idle-policy",
        localAddr: echo.address,
        transport: "tcp",
        codec: true,
      }, 0);
      try {
        await registration.waitReady(READY_MS);
        expect(registration.status()).toBe("connected");
        expect(registration.diagnostics().sdkVersion).toBe(manifest.version);
        expect(await client.listKeys()).toContain("idle-policy");
      } finally {
        await registration.stop();
        await echo.close();
      }
      expect(registration.status()).toBe("stopped");
    });
  },
  30_000,
);

test.skipIf(!hasRelay)(
  "napi nextStatus reports each change until the tunnel settles",
  async () => {
    await withRelay(async (relay) => {
      const client = clientFor(relay);
      const echo = await startEchoServer();
      const registration = await client.register({
        key: "status-events",
        localAddr: echo.address,
        transport: "tcp",
      });
      try {
        let status;
        do {
          status = await registration.nextStatus(READY_MS);
        } while (status !== "connected" && !status.startsWith("failed"));
        expect(status).toBe("connected");
        await registration.stop();
        expect(await registration.nextStatus(READY_MS)).toBe("stopped");
        // A settled tunnel answers at once, which is what ends a listener loop.
        expect(await registration.nextStatus(1)).toBe("stopped");
      } finally {
        await registration.stop();
        await echo.close();
      }
    });
  },
  30_000,
);

test.skipIf(!hasRelay)(
  "napi tcp tunnel round-trips and appears in status",
  async () => {
    await withRelay(async (relay) => {
      const client = clientFor(relay);
      const echo = await startEchoServer();
      const registration = await client.register({
        key: "echo",
        localAddr: echo.address,
        transport: "tcp",
      });
      try {
        await registration.waitReady(READY_MS);
        expect(registration.status()).toBe("connected");
        const diagnostics = registration.diagnostics();
        expect(diagnostics.relayTransport).toBe("tcp");
        const relayDiagnostics = await client.relayDiagnostics();
        expect(relayDiagnostics.transport).toBe("tcp");
        expect(await client.relayQuicPort()).toBeNull();
        expect(relayDiagnostics.connectionsCreated).toBe(0);
        expect(relayDiagnostics.connections).toEqual([]);
        expect(diagnostics.sdkVersion).toBe(manifest.version);
        expect(diagnostics.attempts).toBeGreaterThan(0);
        expect(diagnostics.lastSuccessAgeMs).toBeGreaterThanOrEqual(0);
        expect(diagnostics.activeControlSetups).toBeLessThanOrEqual(8);
        expect(JSON.stringify(diagnostics)).not.toContain(relay.adminKey);
        client.notifyNetworkChange();
        const keys = await client.listKeys();
        expect(keys).toContain("echo");
        const conns = await client.serviceStatus("echo");
        expect(conns.some((conn) => conn.healthy)).toBe(true);
        await client.remoteId();

        const listenAddr = await reserveListenAddress();
        const connection = await client.connect({
          key: "echo",
          localAddr: listenAddr,
          transport: "tcp",
        });
        try {
          await connection.waitReady(READY_MS);
          const payload = Buffer.from("pb-mapper-napi-e2e");
          const echoed = await echoRoundTrip(listenAddr, payload);
          expect(echoed.equals(payload)).toBe(true);

          const admin = client.admin();
          const services = await admin.listServices();
          expect(services.some((svc) => svc.serviceName === "echo")).toBe(true);
          const connections = await admin.listConnections();
          expect(connections.some((conn) => conn.serviceName === "echo")).toBe(
            true,
          );
        } finally {
          await connection.stop();
        }
      } finally {
        await registration.stop();
        await echo.close();
      }
    });
  },
  30_000,
);

test.skipIf(!hasRelay)(
  "napi admin issue/list/show/reveal/revoke",
  async () => {
    await withRelay(async (relay) => {
      const adminClient = clientFor(relay);
      const admin = adminClient.admin();
      const status = await admin.authStatus();
      expect(status.capacity).toBeGreaterThan(0);
      expect(status.serverInstanceId).toBeTruthy();
      // The protocol counters the relay reports: a fresh relay has seen no
      // legacy connection, and its successes are already non-zero because this
      // very call authenticated.
      expect(status.authSuccesses).toBeGreaterThan(0);
      expect(status.authFailures).toBe(0);
      expect(status.lastLegacyConnectionAt ?? null).toBe(null);

      // A page size wider than the wire type must be rejected, not silently
      // narrowed to a one-item page.
      await expect(admin.listKeys(0, 65537)).rejects.toThrow(/out of range/);

      const issued = await admin.issueKey(600, "napi-e2e");
      expect(issued.credential.startsWith("pbmt1_")).toBe(true);
      const listed = await admin.listKeys();
      expect(listed.some((item) => item.keyId === issued.keyId)).toBe(true);

      const shown = await admin.showKey(issued.keyId);
      expect(shown.credential).toBe("");
      const revealed = await admin.revealKey(issued.keyId);
      expect(revealed.credential).toBe(issued.credential);

      const temporary = new Client({
        server: relay.address,
        credential: issued.credential,
      });
      expect(() => temporary.admin()).toThrow(/not an administrator/i);

      const echo = await startEchoServer();
      const registration = await temporary.register({
        key: "echo",
        localAddr: echo.address,
        transport: "tcp",
      });
      try {
        await registration.waitReady(READY_MS);
        await admin.revokeKey(issued.keyId);
        await expect(
          temporary
            .connect({
              key: "echo",
              localAddr: await reserveListenAddress(),
              transport: "tcp",
            })
            .then((connection) => connection.waitReady(3_000)),
        ).rejects.toBeDefined();
      } finally {
        await registration.stop();
        await echo.close();
      }
    });
  },
  30_000,
);

test.skipIf(!hasRelay)(
  "napi admin retires the connections a service is holding",
  async () => {
    await withRelay(async (relay) => {
      const client = clientFor(relay);
      const admin = client.admin();
      const echo = await startEchoServer();
      const registration = await client.register({
        key: "echo",
        localAddr: echo.address,
        transport: "tcp",
      });
      try {
        await registration.waitReady(READY_MS);
        const before = await admin.listConnections();
        expect(before.length).toBeGreaterThan(0);

        // What an operator reaches for when a service's connection quota is
        // full of connections that should have gone away.
        const retired = await admin.retireConnections("echo");
        expect(retired).toBe(before.length);
      } finally {
        await registration.stop();
        await echo.close();
      }
    });
  },
  30_000,
);

test.skipIf(!hasRelay)(
  "napi constructor still requires a real credential even with a live relay",
  async () => {
    await withRelay(async (relay) => {
      expect(
        () =>
          new Client({
            server: relay.address,
            credential: "",
          }),
      ).toThrow(/credential is required/);
      const client = clientFor(relay, ADMIN_KEY);
      expect(client.server()).toBe(relay.address);
    });
  },
  15_000,
);


test.skipIf(!hasRelay)(
  "napi connects direct by default and relay-only opts out per connect",
  async () => {
    const relay = await startRelay({ direct: true });
    const echo = await startEchoServer();
    const client = clientFor(relay);
    // No `allowDirect`: publishers accept direct sessions by default.
    const registration = await client.register({
      key: "direct", localAddr: echo.address, transport: "tcp",
    });
    const connections = [];
    try {
      await registration.waitReady(READY_MS);
      for (const path of [undefined, "relay-only"]) {
        const localAddr = await reserveListenAddress();
        const connection = await client.connect({
          key: "direct", localAddr, transport: "tcp", path,
        });
        connections.push(connection);
        await connection.waitReady(READY_MS);
        const payload = Buffer.alloc(32_000, 0x37);
        expect((await echoRoundTrip(localAddr, payload)).equals(payload)).toBe(true);
        if (path === undefined) {
          const diagnostics = connection.diagnostics();
          expect(diagnostics.direct.directSessions).toBe(1);
          expect(
            diagnostics.events.some((event) => event.kind === "direct_established"),
          ).toBe(true);
          const deadline = Date.now() + READY_MS;
          let observations;
          do {
            observations = JSON.parse(await client.sessionsJson());
            if (observations.length) break;
            await Bun.sleep(20);
          } while (Date.now() < deadline);
          expect(observations.some((s) => s.service === "direct")).toBe(true);
        } else {
          expect(connection.diagnostics().direct ?? null).toBe(null);
        }
      }
    } finally {
      for (const connection of connections) await connection.stop();
      await registration.stop();
      await echo.close();
      await relay.stop();
    }
  },
  30_000,
);
