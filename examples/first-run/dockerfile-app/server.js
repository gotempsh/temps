// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use strict'

// Zero-dependency app built from this directory's Dockerfile by the
// first-run scenario suite, which deploys it from a public git URL.

const http = require('http')

const PORT = parseInt(process.env.PORT || '8080', 10)

http
  .createServer((req, res) => {
    const path = new URL(req.url, 'http://localhost').pathname
    const body = path === '/health' ? { status: 'ok' } : { app: 'first-run-dockerfile' }
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify(body))
  })
  .listen(PORT, () => {
    console.log(JSON.stringify({ level: 'info', msg: 'first-run-dockerfile listening', port: PORT }))
  })
