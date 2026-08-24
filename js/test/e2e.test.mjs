import { expect, test } from "bun:test";
import { Client } from "../index.js";
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
