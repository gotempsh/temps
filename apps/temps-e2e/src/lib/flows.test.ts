// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isTerminalDeployStatus, pollUntil, waitForAppUnavailable } from './flows.ts'

describe('isTerminalDeployStatus', () => {
  test('keeps running deployments non-terminal until readiness completes', () => {
    expect(isTerminalDeployStatus('running')).toEqual({ terminal: false, ok: false })
  })

  test('recognizes completed and failed terminal states', () => {
    expect(isTerminalDeployStatus('completed')).toEqual({ terminal: true, ok: true })
    expect(isTerminalDeployStatus('failed')).toEqual({ terminal: true, ok: false })
  })
})

describe('pollUntil', () => {
  test('waits for an asynchronous result before returning it', async () => {
    let calls = 0
    const result = await pollUntil(
      async () => ++calls,
      (value) => value === 2,
      { timeoutMs: 1000, intervalMs: 1, label: 'asynchronous result' },
    )

    expect(result).toBe(2)
    expect(calls).toBe(2)
  })

  test('reports the last observed state when the deadline expires', async () => {
    await expect(
      pollUntil(async () => [], (value) => value.length === 1, {
        timeoutMs: 100,
        intervalMs: 1,
        label: 'auto-created monitor',
      }),
    ).rejects.toThrow(/auto-created monitor did not converge.*last: \[\]/)
  })
})

describe('waitForAppUnavailable', () => {
  const consoleHtml = '<!doctype html><html><head><title>Temps</title></head></html>'
  const unavailableHtml = '<!doctype html><html><head><title>Service Unavailable</title></head></html>'

  test('waits for a genuine 503 after the application stops serving', async () => {
    let requests = 0
    const server = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      fetch() {
        requests++
        return requests === 1
          ? new Response('version A', { status: 200 })
          : new Response(unavailableHtml, { status: 503 })
      },
    })
    try {
      await waitForAppUnavailable({ url: server.url.href, timeoutMs: 2000, intervalMs: 5 })
      expect(requests).toBeGreaterThanOrEqual(2)
    } finally {
      server.stop(true)
    }
  })

  for (const { label, status, body } of [
    { label: 'console HTTP 200', status: 200, body: consoleHtml },
    { label: 'console HTTP 503', status: 503, body: consoleHtml },
    { label: 'application HTTP 200', status: 200, body: 'version A' },
  ]) {
    test(`rejects ${label} as proof of an unavailable application`, async () => {
      const server = Bun.serve({
        hostname: '127.0.0.1',
        port: 0,
        fetch: () => new Response(body, { status }),
      })
      try {
        await expect(
          waitForAppUnavailable({ url: server.url.href, timeoutMs: 200, intervalMs: 5 }),
        ).rejects.toThrow(/expected .*HTTP 503 without the Temps console HTML/)
      } finally {
        server.stop(true)
      }
    })
  }

  test('rejects a transport refusal as proof of an unavailable application', async () => {
    const server = Bun.serve({
      hostname: '127.0.0.1',
      port: 0,
      fetch: () => new Response(unavailableHtml, { status: 503 }),
    })
    const url = server.url.href
    server.stop(true)
    await expect(
      waitForAppUnavailable({ url, timeoutMs: 200, intervalMs: 5 }),
    ).rejects.toThrow(/expected .*HTTP 503 without the Temps console HTML/)
  })
})
