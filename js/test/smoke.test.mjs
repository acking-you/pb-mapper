import { expect, test } from "bun:test";
import { Client } from "../index.js";

test("constructor rejects an empty credential", () => {
  expect(
    () =>
      new Client({
        server: "127.0.0.1:7666",
        credential: "",
      }),
  ).toThrow(/credential is required/);
});

test("constructor accepts an administrator key", () => {
  const client = new Client({
    server: "127.0.0.1:7666",
    credential: "0123456789abcdefghijklmnopqrstuv",
  });
  expect(client.server()).toBe("127.0.0.1:7666");
  expect(client.admin()).toBeTruthy();
});

test("admin() refuses a temporary-looking non-admin key", () => {
  expect(
    () =>
      new Client({
        server: "127.0.0.1:7666",
        credential: "short",
      }),
  ).toThrow(/32 bytes/);
});

test("idle deadline rejects invalid JS numbers and UDP before spawning a worker", async () => {
  const client = new Client({
    server: "127.0.0.1:7666",
    credential: "0123456789abcdefghijklmnopqrstuv",
  });
  const request = { key: "probe", localAddr: "127.0.0.1:1", transport: "tcp" };
  for (const value of [-1, 0.5, NaN, Infinity, 2 ** 53]) {
    await expect(client.registerWithIdleTimeout(request, value)).rejects.toThrow(/safe integer/);
  }
  await expect(client.registerWithIdleTimeout({ ...request, transport: "udp" }, 0))
    .rejects.toThrow(/requires TCP/);
});
