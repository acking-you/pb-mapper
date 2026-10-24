// Remote control for an agent harness: one scoped credential per session.
//
//   PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<administrator key> \
//     node examples/remote-control.mjs
//
// The operator issues a short-lived credential; the device publishes its
// control endpoint with it; the controller reaches that endpoint on a local
// port with the same credential, peer to peer where possible; revoking the
// credential ends the session. Device and controller share this process only
// to keep the example self-contained: each is its own `Client`.
import net from "node:net";
import { Client } from "pb-mapper";

const server = process.env.PB_MAPPER_SERVER;
const admin = new Client({ server, credential: process.env.MSG_HEADER_KEY }).admin();

// A stand-in for the harness's HTTP or WebSocket control port.
const endpoint = net.createServer((socket) => socket.end("hello from the device\n"));
await new Promise((resolve) => endpoint.listen(0, "127.0.0.1", resolve));
const endpointAddr = `127.0.0.1:${endpoint.address().port}`;

// 1. One credential for this session, expiring on its own if never revoked.
const issued = await admin.issueKey(15 * 60, "agent-session");

// 2. Device side.
const device = new Client({ server, credential: issued.credential, keepAlive: true });
const registration = await device.register({
  key: "harness", localAddr: endpointAddr, transport: "tcp", codec: true,
});
await registration.waitReady();

// 3. Controller side, possibly on another continent.
const controller = new Client({ server, credential: issued.credential, keepAlive: true });
const localAddr = "127.0.0.1:18080";
const connection = await controller.connect({ key: "harness", localAddr, transport: "tcp" });
await connection.waitReady();
console.log(`session namespace ${issued.keyId} sees`, await controller.listKeys());

const reply = await new Promise((resolve, reject) => {
  const [host, port] = localAddr.split(":");
  const socket = net.connect(Number(port), host);
  let text = "";
  socket.on("data", (chunk) => (text += chunk));
  socket.on("end", () => resolve(text.trim()));
  socket.on("error", reject);
});
const { direct } = connection.diagnostics();
console.log(`controller got "${reply}" via ${direct?.directSessions ? "direct" : "relay"}`);

// 4. End the session.
await admin.revokeKey(issued.keyId);
await connection.stop();
await registration.stop();
endpoint.close();
console.log(`credential ${issued.keyId} revoked`);
