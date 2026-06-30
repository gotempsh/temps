'use strict';

const http = require('http');
const Sentry = require('@sentry/node');

const ROLE = process.env.ROLE || 'backend'; // 'gateway' | 'backend'
const PORT = parseInt(process.env.PORT || '8080', 10);
const TARGET = process.env.TARGET || ''; // gateway: downstream fqdn (e.g. production.obs-backend.temps.local)
const cpHost = process.env.CP_INTERNAL_HOST;

// Same dev-cluster shim as tracing.js: the injected SENTRY_DSN points at
// app.localho.st (loopback inside containers); rewrite to the reachable CP.
let dsn = process.env.SENTRY_DSN || '';
if (dsn && cpHost) {
  dsn = dsn.replace('app.localho.st', cpHost).replace('https://', 'http://');
}
if (dsn) {
  Sentry.init({
    dsn,
    environment: process.env.OTEL_SERVICE_NAME || ROLE,
    // We export traces via OTLP; let Sentry handle errors only.
    tracesSampleRate: 0,
  });
  console.log('sentry: initialised for', process.env.OTEL_SERVICE_NAME || ROLE);
} else {
  console.log('sentry: no DSN, error reporting disabled');
}

const node = {
  name: process.env.TEMPS_NODE_NAME || null,
  id: process.env.TEMPS_NODE_ID || null,
  replica: process.env.TEMPS_REPLICA || null,
};

function callTarget() {
  return new Promise((resolve, reject) => {
    const req = http.get('http://' + TARGET + '/', { timeout: 5000 }, (r) => {
      let d = '';
      r.on('data', (c) => (d += c));
      r.on('end', () => resolve({ status: r.statusCode, body: safeJson(d) }));
    });
    req.on('error', reject);
    req.on('timeout', () => req.destroy(new Error('timeout calling ' + TARGET)));
  });
}

function safeJson(s) {
  try {
    return JSON.parse(s);
  } catch {
    return s.slice(0, 200);
  }
}

const server = http.createServer(async (req, res) => {
  const url = req.url || '/';

  if (url.startsWith('/health')) {
    res.writeHead(200, { 'content-type': 'text/plain' });
    return res.end('ok');
  }

  // Deliberate failure path: captured by Sentry -> Temps error tracking, and
  // returned as a 5xx so it also shows up as an error in traces/logs.
  if (url.startsWith('/boom')) {
    const err = new Error('synthetic /boom failure in ' + ROLE + ' on ' + (node.name || 'local'));
    err.node = node;
    Sentry.captureException(err);
    console.error('error:', err.message);
    res.writeHead(500, { 'content-type': 'application/json' });
    return res.end(JSON.stringify({ error: err.message, role: ROLE, node }));
  }

  // gateway: fan out to the downstream backend (creates the client span that
  // links the two services in one trace). backend: just describe itself.
  let downstream = null;
  if (ROLE === 'gateway' && TARGET) {
    try {
      downstream = await callTarget();
    } catch (e) {
      Sentry.captureException(e);
      downstream = { error: (e && e.message) || String(e) };
    }
  }

  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(
    JSON.stringify(
      {
        service: process.env.OTEL_SERVICE_NAME || ROLE,
        role: ROLE,
        node,
        downstream,
      },
      null,
      2
    )
  );
});

server.listen(PORT, () => {
  console.log(ROLE + ' listening on :' + PORT + (TARGET ? ' -> ' + TARGET : ''));
});

// Self-driving traffic so traces + errors flow continuously without manual
// curls. The gateway hits its own endpoints; HttpInstrumentation traces both
// the request it serves and the downstream call it makes. Every 4th request
// targets /boom to keep the error stream populated.
if (ROLE === 'gateway') {
  let n = 0;
  setInterval(() => {
    n += 1;
    const path = n % 4 === 0 ? '/boom' : '/';
    http
      .get('http://127.0.0.1:' + PORT + path, (r) => r.resume())
      .on('error', () => {});
  }, 3000);
}
