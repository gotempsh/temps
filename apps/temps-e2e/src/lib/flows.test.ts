// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { appUnavailableMismatch, isTerminalDeployStatus, pollUntil } from './flows.ts'

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

describe('appUnavailableMismatch', () => {
  const outagePage =
    '<!DOCTYPE html><html><head><meta charset="utf-8"><title>Service Unavailable</title></head><body></body></html>'
  const consolePage = '<!DOCTYPE html><html><head><title>Temps</title></head><body><div id="root"></div></body></html>'

  test('accepts the proxy outage page for a paused app', () => {
    expect(
      appUnavailableMismatch({ status: 503, body: outagePage, cacheControl: 'private, no-store' }, 'APP-A'),
    ).toBeNull()
  })

  test('rejects the console fallback on the application hostname', () => {
    expect(
      appUnavailableMismatch({ status: 200, body: consolePage, cacheControl: null }, 'APP-A'),
    ).toContain('console fallback')
  })

  test('rejects the app body still being served', () => {
    expect(appUnavailableMismatch({ status: 200, body: 'APP-A', cacheControl: null }, 'APP-A')).toContain(
      'still served the application body',
    )
  })

  test('rejects a 503 that is not the proxy outage page', () => {
    expect(
      appUnavailableMismatch({ status: 503, body: 'upstream overloaded', cacheControl: 'private, no-store' }),
    ).toContain('without the proxy outage page')
  })

  test('rejects a cacheable outage page', () => {
    expect(appUnavailableMismatch({ status: 503, body: outagePage, cacheControl: 'public, max-age=60' })).toContain(
      'Cache-Control',
    )
  })

  test('rejects other error statuses', () => {
    expect(appUnavailableMismatch({ status: 502, body: outagePage, cacheControl: 'private, no-store' })).toBe(
      'expected HTTP 503, got HTTP 502',
    )
  })
})
