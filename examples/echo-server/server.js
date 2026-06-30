'use strict'

// Minimal, zero-dependency echo server for Temps deployment demos.
//
// Every request gets a JSON response reflecting the request (method, path,
// query, headers, body) and the container's environment — including the
// node-identity variables Temps injects on a multi-node cluster
// (TEMPS_NODE_NAME / TEMPS_NODE_ID / TEMPS_REPLICA). Each request also emits a
// structured JSON log line to stdout so it shows up in Temps log history.

const http = require('http')
const os = require('os')

const PORT = parseInt(process.env.PORT || '8080', 10)

// Node identity injected by Temps when deployed across a multi-node cluster.
const node = {
  name: process.env.TEMPS_NODE_NAME || null,
  id: process.env.TEMPS_NODE_ID || null,
  replica: process.env.TEMPS_REPLICA || null,
}

function readBody(req) {
  return new Promise((resolve) => {
    const chunks = []
    req.on('data', (c) => chunks.push(c))
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    req.on('error', () => resolve(''))
  })
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host || 'localhost'}`)
  const hasBody = ['POST', 'PUT', 'PATCH'].includes(req.method)
  const body = hasBody ? await readBody(req) : ''

  const payload = {
    service: 'temps-echo',
    hostname: os.hostname(),
    node,
    request: {
      method: req.method,
      path: url.pathname,
      query: Object.fromEntries(url.searchParams),
      headers: req.headers,
      remoteAddress: req.socket.remoteAddress,
      body: body || undefined,
    },
    env: process.env,
    timestamp: new Date().toISOString(),
  }

  // Structured log line per request — appears in Temps log history, tagged with
  // the container + node it ran on.
  console.log(
    JSON.stringify({
      level: 'info',
      msg: `${req.method} ${url.pathname}`,
      node: node.name,
      replica: node.replica,
      status: 200,
    }),
  )

  res.writeHead(200, { 'content-type': 'application/json' })
  res.end(JSON.stringify(payload, null, 2))
})

server.listen(PORT, () => {
  console.log(
    JSON.stringify({
      level: 'info',
      msg: `temps-echo listening on :${PORT}`,
      node: node.name,
      id: node.id,
      replica: node.replica,
      hostname: os.hostname(),
    }),
  )
})
