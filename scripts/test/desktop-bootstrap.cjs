// Loopback-only bootstrap for the explicit UI acceptance entry. The directory
// must be a dedicated `deployed ... serve` output, never the user's app profile.
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

if (process.argv.length !== 3) {
  throw new Error('Usage: node desktop-bootstrap.cjs DEDICATED_TEST_DIRECTORY');
}
const file = path.join(path.resolve(process.argv[2]), 'controller.json');
http.createServer(async (request, response) => {
  if (request.method !== 'GET' || request.url !== '/controller.json') {
    response.writeHead(404).end();
    return;
  }
  try {
    const data = await fs.readFile(file);
    if (data.length > 16 * 1024) throw new Error('Oversized bootstrap');
    response.writeHead(200, {
      'Content-Type': 'application/json',
      'Cache-Control': 'no-store',
    }).end(data);
  } catch {
    response.writeHead(503).end('not ready');
  }
}).listen(18747, '127.0.0.1', () => process.stdout.write('bootstrap_ready\n'));
