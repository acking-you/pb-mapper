// Reach a registered service on a local port and listen to the tunnel.
//
//   PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//     node examples/connect.mjs web 127.0.0.1:9090 [tcp|udp] [--relay-only]
//
// Each connection to the local port tries direct first and falls back to the
// relay. This prints every status change and every new tunnel event.
import { Client } from "pb-mapper";

const [key = "web", localAddr = "127.0.0.1:9090", transport = "tcp"] =
  process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const path = process.argv.includes("--relay-only") ? "relay-only" : "auto";

const client = new Client({
  server: process.env.PB_MAPPER_SERVER,
  credential: process.env.MSG_HEADER_KEY,
  keepAlive: true,
});
const connection = await client.connect({ key, localAddr, transport, path });
process.once("SIGINT", () => connection.stop());

// Events: print each one once, by sequence number.
let seen = 0;
const events = setInterval(() => {
  const { events, direct } = connection.diagnostics();
  for (const event of events.filter((e) => e.seq > seen)) {
    seen = event.seq;
    const detail = event.setupMs ?? event.latencyMs ?? event.stage ?? event.failure ?? "";
    console.log(`event: ${event.kind} ${detail}`);
  }
  if (direct?.attempt) {
    console.log(`direct attempt: ${direct.attempt.stage} for ${direct.attempt.elapsedMs} ms`);
  }
}, 500);

// Status: resolves on every change and ends once the tunnel settles.
let status = connection.status();
console.log(`\`${key}\` at ${localAddr} (${path}): ${status}`);
while (status !== "stopped" && !status.startsWith("failed")) {
  status = await connection.nextStatus();
  console.log(`status: ${status}`);
}
clearInterval(events);
console.log(JSON.stringify(connection.diagnostics(), null, 2));
