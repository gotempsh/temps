// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use strict'

// Zero-dependency Node app for the first-run scenario suite
// (apps/temps-e2e `first-run-scenario`). It is deployed by uploading this
// directory as source, so Temps has to detect a Node preset and build it.
//
//   GET /        -> {"app":"first-run-node"}
//   GET /health  -> {"status":"ok"}
//   GET /env     -> which managed-service variables are present and whether
//                   the host:port each one points at accepts a TCP connection.
//                   Values are never echoed back: they carry credentials.

const http = require('http')
const net = require('net')

const PORT = parseInt(process.env.PORT || '3000', 10)
const SERVICE_VARIABLES = ['POSTGRES_URL', 'REDIS_URL']

function probe(value) {
  return new Promise((resolve) => {
    let target
    try {
      target = new URL(value)
    } catch {
      resolve({ present: true, parsed: false, reachable: false })
      return
    }
    const defaultPort = target.protocol.startsWith('redis') ? 6379 : 5432
    const socket = net.connect({
      host: target.hostname,
      port: parseInt(target.port || String(defaultPort), 10),
    })
    const finish = (reachable) => {
      socket.destroy()
      resolve({ present: true, parsed: true, reachable })
    }
    socket.setTimeout(3000, () => finish(false))
    socket.once('connect', () => finish(true))
    socket.once('error', () => finish(false))
  })
}

async function environmentReport() {
  const report = {}
  for (const name of SERVICE_VARIABLES) {
    const value = process.env[name]
    report[name] = value ? await probe(value) : { present: false, parsed: false, reachable: false }
  }
  return report
}

function send(res, status, body) {
  res.writeHead(status, { 'content-type': 'application/json' })
  res.end(JSON.stringify(body))
}

http
  .createServer(async (req, res) => {
    const path = new URL(req.url, 'http://localhost').pathname
    if (path === '/health') return send(res, 200, { status: 'ok' })
    if (path === '/env') return send(res, 200, await environmentReport())
    if (path === '/') return send(res, 200, { app: 'first-run-node' })
    return send(res, 404, { error: 'not found' })
  })
  .listen(PORT, () => {
    console.log(JSON.stringify({ level: 'info', msg: 'first-run-node listening', port: PORT }))
  })
