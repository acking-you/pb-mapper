// The credential lifecycle and the relay inventory, with the administrator key.
//
//   PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<administrator key> \
//     node examples/admin.mjs
import { Client } from "pb-mapper";

const client = new Client({
  server: process.env.PB_MAPPER_SERVER,
  credential: process.env.MSG_HEADER_KEY,
});
// Throws locally for a temporary credential.
const admin = client.admin();

// Issue, inspect, renew, revoke. Secrets are re-derived, not stored.
const issued = await admin.issueKey(3600, "node-example");
console.log(`issued key ${issued.keyId}, expires at ${issued.expiresAt}`);
const shown = await admin.showKey(issued.keyId); // metadata only
const revealed = await admin.revealKey(issued.keyId); // with the secret
console.log(`label=${shown.label} same secret: ${revealed.credential === issued.credential}`);
await admin.renewKey(issued.keyId, 7200);
for (const key of await admin.listKeys()) {
  console.log(`  key ${key.keyId} ${key.label ?? ""} ${key.state}`);
}
await admin.revokeKey(issued.keyId);
console.log(`collected ${await admin.gcKeys()} expired or revoked credential(s)`);

// Every namespace's services and control connections.
for (const service of await admin.listServices()) {
  console.log(`service \`${service.serviceName}\` in ${service.namespace}: ${service.connectionCount} connection(s)`);
}
for (const conn of await admin.listConnections()) {
  if (!conn.healthy) {
    // A healthy publisher reconnects, so retiring is a nudge, not a shutdown.
    await admin.retireConnections(conn.serviceName, conn.keyId, conn.connId);
  }
}

const auth = await admin.authStatus();
console.log(`${auth.activeKeys}/${auth.capacity} credentials active, legacy protocol ${auth.legacyProtocol}`);
