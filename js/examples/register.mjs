// Publish a private service on the relay.
//
//   PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//     node examples/register.mjs web 127.0.0.1:8080 [tcp|udp] [--relay-only]
//
// Direct sessions are allowed by default; `--relay-only` keeps every session
// to this service on the relay. The registration survives relay restarts and
// network changes until it is stopped.
import { Client } from "pb-mapper";

const [key = "web", localAddr = "127.0.0.1:8080", transport = "tcp"] =
  process.argv.slice(2).filter((arg) => !arg.startsWith("--"));
const relayOnly = process.argv.includes("--relay-only");

const client = new Client({
  server: process.env.PB_MAPPER_SERVER,
  credential: process.env.MSG_HEADER_KEY,
  keepAlive: true,
  portMapping: process.argv.includes("--port-mapping"),
});
const registration = await client.register({
  key,
  localAddr,
  transport,
  allowDirect: !relayOnly,
  // AES-256-GCM on relayed data; direct sessions are always encrypted by QUIC.
  codec: true,
});
await registration.waitReady();
console.log(`publishing ${transport} \`${key}\` from ${localAddr}`);

process.once("SIGINT", async () => {
  await registration.stop();
  process.exit(0);
});
