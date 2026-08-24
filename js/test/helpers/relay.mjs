import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const ADMIN_KEY = "0123456789abcdefghijklmnopqrstuv";

const repoRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../..",
);

export function findPbMapperBinary() {
  if (process.env.PB_MAPPER_BIN) {
    return process.env.PB_MAPPER_BIN;
  }
  for (const candidate of [
    path.join(repoRoot, "target/debug/pb-mapper"),
    path.join(repoRoot, "target/release/pb-mapper"),
  ]) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close((error) => {
        if (error) {
          reject(error);
        } else {
          resolve(port);
        }
      });
    });
    server.on("error", reject);
  });
}

function waitForPort(port, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const socket = net.connect({ host: "127.0.0.1", port }, () => {
        socket.end();
        resolve();
      });
      socket.on("error", () => {
        socket.destroy();
        if (Date.now() > deadline) {
          reject(new Error(`relay on 127.0.0.1:${port} did not become ready`));
          return;
        }
        setTimeout(attempt, 50);
      });
    };
    attempt();
  });
}

export function startEchoServer() {
  return new Promise((resolve, reject) => {
    const server = net.createServer((socket) => {
      socket.pipe(socket);
    });
    server.listen(0, "127.0.0.1", () => {
      resolve({
        address: `127.0.0.1:${server.address().port}`,
        port: server.address().port,
        close: () =>
          new Promise((done) => {
            server.close(() => done());
          }),
      });
    });
    server.on("error", reject);
  });
}

export function reserveListenAddress() {
  return reservePort().then((port) => `127.0.0.1:${port}`);
}

export async function echoRoundTrip(address, payload, timeoutMs = 5_000) {
  const [host, port] = address.split(":");
  return new Promise((resolve, reject) => {
    let socket;
    const timer = setTimeout(() => {
      socket?.destroy();
      reject(new Error(`echo round-trip timed out after ${timeoutMs}ms`));
    }, timeoutMs);
    const chunks = [];
    socket = net.connect({ host, port: Number(port) }, () => {
      socket.write(payload);
    });
    socket.on("data", (chunk) => {
      chunks.push(chunk);
      const got = Buffer.concat(chunks);
      if (got.length >= payload.length) {
        clearTimeout(timer);
        socket.end();
        resolve(got.subarray(0, payload.length));
      }
    });
    socket.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
}

export async function startRelay() {
  const bin = findPbMapperBinary();
  if (!bin) {
    return null;
  }
  const stateDir = fs.mkdtempSync(path.join(os.tmpdir(), "pb-mapper-napi-e2e-"));
  fs.writeFileSync(path.join(stateDir, "admin.key"), `${ADMIN_KEY}\n`, {
    mode: 0o600,
  });
  const port = await reservePort();
  const logs = [];
  const child = spawn(bin, ["server", "--port", String(port)], {
    env: {
      ...process.env,
      MSG_HEADER_KEY: ADMIN_KEY,
      PB_MAPPER_AUTH_STATE_DIR: stateDir,
      RUST_LOG: process.env.RUST_LOG ?? "error",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const onLog = (chunk) => logs.push(chunk.toString());
  child.stdout.on("data", onLog);
  child.stderr.on("data", onLog);
  const exitError = new Promise((_, reject) => {
    child.on("exit", (code, signal) => {
      reject(
        new Error(
          `pb-mapper server exited (code=${code} signal=${signal}): ${logs.join("")}`,
        ),
      );
    });
  });
  try {
    await Promise.race([waitForPort(port), exitError]);
  } catch (error) {
    child.kill("SIGKILL");
    throw error;
  }
  child.removeAllListeners("exit");
  return {
    port,
    address: `127.0.0.1:${port}`,
    adminKey: ADMIN_KEY,
    async stop() {
      if (!child.killed) {
        child.kill("SIGTERM");
        await new Promise((resolve) => {
          const timer = setTimeout(() => {
            child.kill("SIGKILL");
            resolve();
          }, 2000);
          child.once("exit", () => {
            clearTimeout(timer);
            resolve();
          });
        });
      }
      fs.rmSync(stateDir, { recursive: true, force: true });
    },
  };
}
