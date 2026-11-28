#!/usr/bin/env node
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Entry point launched by the MCP bundle manifest: newline-delimited
 * JSON-RPC on stdin/stdout. stdout carries protocol messages only; every
 * diagnostic goes to stderr as one JSON object per line.
 */

import { createInterface } from 'node:readline'
import { createServer } from './bridge.js'

function log(entry) {
  process.stderr.write(`${JSON.stringify({ ts: new Date().toISOString(), ...entry })}\n`)
}

const server = createServer({ env: process.env, log })

const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
let pending = 0
let closing = false

function maybeExit() {
  if (closing && pending === 0) process.exit(0)
}

input.on('line', (line) => {
  pending += 1
  server
    .handleLine(line)
    .then((response) => {
      if (response) process.stdout.write(`${JSON.stringify(response)}\n`)
    })
    .catch((error) => {
      log({ level: 'error', msg: 'failed to write MCP response', error: error instanceof Error ? error.message : String(error) })
    })
    .finally(() => {
      pending -= 1
      maybeExit()
    })
})

input.on('close', () => {
  closing = true
  maybeExit()
})
