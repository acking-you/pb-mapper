// Publish a private service on the relay.
//
//   PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//     node examples/register.mjs web 127.0.0.1:8080 [tcp|udp] [--relay-only] [--port-mapping[=always]]
//
// Direct sessions are allowed by default; `--relay-only` keeps every session
// to this service on the relay. `--port-mapping` asks the router to map the
// direct UDP port once punching through fails; `=always`, from the start. The registration survives relay restarts and
// network changes until it is stopped.
import { Client } from "pb-mapper";

const [key = "web", localAddr = "127.0.0.1:8080", transport = "tcp"] =
  process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const relayOnly = process.argv.includes("--relay-only");

const client = new Client({
  server: process.env.PB_MAPPER_SERVER,
  credential: process.env.MSG_HEADER_KEY,
  keepAlive: true,
});
const registration = await client.register({
  key,
  localAddr,
  transport,
  allowDirect: !relayOnly,
  portMapping: portMappingArg(),
  // AES-256-GCM on relayed data; direct sessions are always encrypted by QUIC.
  codec: true,
});
await registration.waitReady();
console.log(`publishing ${transport} \`${key}\` from ${localAddr}`);

process.once("SIGINT", async () => {
  await registration.stop();
  process.exit(0);
});

/** `--port-mapping` is "auto"; `--port-mapping=always` is "always". */
function portMappingArg() {
  const flag = process.argv.find((arg) => arg.startsWith("--port-mapping"));
  return flag ? (flag.split("=")[1] ?? "auto") : "off";
}
