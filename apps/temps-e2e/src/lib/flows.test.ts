// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isTerminalDeployStatus, pollUntil } from './flows.ts'

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
