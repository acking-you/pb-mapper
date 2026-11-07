// Loopback-only bootstrap for the explicit UI acceptance entry. The directory
// must be a dedicated `deployed ... serve` output, never the user's app profile.
//
// GET /controller.json hands the viewer its setup. PUT /report takes the
// viewer's acceptance report back and saves it under a name chosen here, so
// nothing is read from the app's private storage (a release APK cannot be
// read with `run-as`).
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

const SETUP_MAX_BYTES = 16 * 1024;
const REPORT_MAX_BYTES = 8 * 1024 * 1024;
const LOOPBACK = new Set(['127.0.0.1', '::1', '::ffff:127.0.0.1']);

if (process.argv.length !== 3) {
  throw new Error('Usage: node desktop-bootstrap.cjs DEDICATED_TEST_DIRECTORY');
}
const directory = path.resolve(process.argv[2]);
const file = path.join(directory, 'controller.json');

async function sendSetup(response) {
  try {
    const data = await fs.readFile(file);
    if (data.length > SETUP_MAX_BYTES) throw new Error('Oversized bootstrap');
    response.writeHead(200, {
      'Content-Type': 'application/json',
      'Cache-Control': 'no-store',
    }).end(data);
  } catch {
    response.writeHead(503).end('not ready');
  }
}

// Resolves to the body, or to null as soon as it exceeds REPORT_MAX_BYTES;
// the rest is discarded while the refusal is sent.
function readBounded(request) {
  return new Promise((resolve, reject) => {
    if (Number(request.headers['content-length']) > REPORT_MAX_BYTES) {
      resolve(null);
      return;
    }
    const chunks = [];
    let length = 0;
    request.on('data', (chunk) => {
      if (length > REPORT_MAX_BYTES) return;
      length += chunk.length;
      if (length > REPORT_MAX_BYTES) {
        chunks.length = 0;
        resolve(null);
      } else {
        chunks.push(chunk);
      }
    });
    request.on('end', () => resolve(Buffer.concat(chunks, length)));
    request.on('error', reject);
  });
}

// The first free report-<n>.json; `wx` keeps concurrent uploads apart.
async function saveReport(body) {
  for (let n = 1; ; n++) {
    const target = path.join(directory, `report-${n}.json`);
    try {
      await fs.writeFile(target, body, { flag: 'wx' });
      return target;
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
    }
  }
}

async function receiveReport(request, response) {
  try {
    const body = await readBounded(request);
    if (body === null) {
      response.writeHead(413, { Connection: 'close' }).end('report too large');
      return;
    }
    try {
      JSON.parse(body.toString('utf8'));
    } catch {
      response.writeHead(400).end('report is not JSON');
      return;
    }
    const saved = await saveReport(body);
    process.stdout.write(`report_saved ${saved}\n`);
    response.writeHead(201).end();
  } catch {
    if (!response.headersSent) response.writeHead(500).end('report not saved');
  }
}

http.createServer((request, response) => {
  if (!LOOPBACK.has(request.socket.remoteAddress)) {
    response.writeHead(403, { Connection: 'close' }).end();
  } else if (request.method === 'GET' && request.url === '/controller.json') {
    sendSetup(response);
  } else if (request.method === 'PUT' && request.url === '/report') {
    receiveReport(request, response);
  } else {
    response.writeHead(404).end();
  }
}).listen(18747, '127.0.0.1', () => process.stdout.write('bootstrap_ready\n'));
