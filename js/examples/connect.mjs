// Reach a registered service on a local port and listen to the tunnel.
//
//   PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//     node examples/connect.mjs web 127.0.0.1:9090 [tcp|udp] [--relay-only] [--port-mapping[=always]]
//
// Each connection to the local port tries direct first and falls back to the
// relay. `--port-mapping` asks the router to map the direct UDP port once
// punching through fails; `=always`, from the start. This prints every
// status change and every tunnel event.
import { Client } from "pb-mapper";

const [key = "web", localAddr = "127.0.0.1:9090", transport = "tcp"] =
  process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const path = process.argv.includes("--relay-only") ? "relay-only" : "auto";

const client = new Client({
  server: process.env.PB_MAPPER_SERVER,
  credential: process.env.MSG_HEADER_KEY,
  keepAlive: true,
});
const flag = process.argv.find((arg) => arg.startsWith("--port-mapping"));
const portMapping = flag ? (flag.split("=")[1] ?? "auto") : "off";
const connection = await client.connect({ key, localAddr, transport, path, portMapping });
process.once("SIGINT", () => connection.stop());

// Events: the outcomes kept so far, then each one as it happens. `next()`
// resolves null once the tunnel has stopped.
const events = connection.subscribeEvents();
const monitoring = (async () => {
  for (;;) {
    try {
      const event = await events.next();
      if (event === null) break;
      console.log(JSON.stringify(event));
    } catch (error) {
      // Fell behind: say how far, and take the current state from a snapshot.
      console.error(error.message);
      console.log(JSON.stringify(connection.diagnostics()));
    }
  }
})();

// Status: resolves on every change and ends once the tunnel settles.
let status = connection.status();
console.log(`\`${key}\` at ${localAddr} (${path}): ${status}`);
while (status !== "stopped" && !status.startsWith("failed")) {
  status = await connection.nextStatus();
  console.log(`status: ${status}`);
}
await monitoring;
events.close();
console.log(JSON.stringify(connection.diagnostics(), null, 2));
