// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatLag,
  formatSystemReserved,
  nodeStateStreamSummary,
  parseSystemReserved,
  resourcePolicySummary,
  systemReservedDefaultsText,
  type NodeStateStatus,
} from './node-workload'

function stream(overrides: Partial<NodeStateStatus>): NodeStateStatus {
  return {
    configured: true,
    reason: null,
    watermark_lag_ms: 0,
    reads: 10,
    full_snapshots: 2,
    markers_swept: 0,
    ...overrides,
  }
}

describe('system reserve', () => {
  test('formats whole vCPU and GB', () => {
    expect(formatSystemReserved({ cpu_millis: 500, memory_mb: 1536 })).toBe(
      '500m CPU, 1.5 GB memory'
    )
    expect(formatSystemReserved({ cpu_millis: 1000, memory_mb: 256 })).toBe(
      '1 vCPU, 256 MB memory'
    )
  })

  test('defaults differ for the control plane and a worker', () => {
    expect(systemReservedDefaultsText(0)).toContain('external database')
    expect(systemReservedDefaultsText(4)).toContain('250m CPU, 256 MB')
  })

  test('parses valid inputs', () => {
    expect(parseSystemReserved('250', ' 512 ')).toEqual({
      ok: true,
      value: { cpu_millis: 250, memory_mb: 512 },
    })
  })

  test('rejects negative, fractional, empty and too-large values', () => {
    expect(parseSystemReserved('-1', '1').ok).toBe(false)
    expect(parseSystemReserved('1.5', '1').ok).toBe(false)
    expect(parseSystemReserved('', '1').ok).toBe(false)
    const big = parseSystemReserved('256001', '1')
    expect(big.ok).toBe(false)
    if (!big.ok) expect(big.error).toContain('256000')
  })

  test('summarises the policy with the reserve', () => {
    expect(
      resourcePolicySummary({
        node_id: 1,
        resource_policy: 'enforced',
        system_reserved: { cpu_millis: 250, memory_mb: 256 },
        system_reserved_is_default: true,
      })
    ).toContain('is refused')
  })
})

describe('nodeStateStreamSummary', () => {
  test('not configured shows the server reason', () => {
    const summary = nodeStateStreamSummary(
      stream({ configured: false, reason: 'the workload plane is disabled' })
    )
    expect(summary.tone).toBe('off')
    expect(summary.detail).toBe('the workload plane is disabled')
  })

  test('a large lag is a warning', () => {
    const summary = nodeStateStreamSummary(stream({ watermark_lag_ms: 12_000 }))
    expect(summary.tone).toBe('warning')
    expect(summary.label).toBe('Lagging 12 s')
  })

  test('a healthy stream reports its lag', () => {
    const summary = nodeStateStreamSummary(stream({ watermark_lag_ms: 40 }))
    expect(summary.tone).toBe('ok')
    expect(summary.detail).toContain('40 ms')
  })

  test('formatLag picks a unit', () => {
    expect(formatLag(1500)).toBe('1.5 s')
    expect(formatLag(180_000)).toBe('3 min')
  })
})
