// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  buildStatusRows,
  formatBytes,
  formatPercent,
  isCumulativeCounter,
  toRatePerSecond,
} from './index.js'

const t = (secs: number) => new Date(1_700_000_000_000 + secs * 1000).toISOString()

describe('formatBytes', () => {
  test('binary units with sensible precision', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(1536)).toBe('1.50 KiB')
    expect(formatBytes(7.76 * 1024 ** 3)).toBe('7.76 GiB')
    expect(formatBytes(null)).toBe('-')
  })
})

describe('toRatePerSecond', () => {
  test('divides per-bucket increases by the bucket width', () => {
    const rate = toRatePerSecond(
      [
        { time: t(0), value: 6000 },
        { time: t(60), value: 12000 },
      ],
      30
    )
    expect(rate.map((p) => p.value)).toEqual([100, 200])
  })

  test('single point uses fallback step and negatives floor at zero', () => {
    expect(toRatePerSecond([{ time: t(0), value: 300 }], 30)[0]?.value).toBe(10)
    expect(toRatePerSecond([{ time: t(0), value: -3 }], 30)[0]?.value).toBe(0)
  })
})

describe('isCumulativeCounter', () => {
  test('matches the server-side _total / _count convention', () => {
    expect(isCumulativeCounter('node.network_rx_bytes_total')).toBe(true)
    expect(isCumulativeCounter('node.cpu_percent')).toBe(false)
  })
})

describe('buildStatusRows', () => {
  test('renders only the rows whose metrics are present', () => {
    const rows = buildStatusRows({
      'node.cpu_percent': 2.5,
      'node.memory_used_bytes': 2 * 1024 ** 3,
      'node.memory_total_bytes': 8 * 1024 ** 3,
      'node.memory_percent': 25,
      'node.network_rx_bytes_total': 1024,
      'node.network_tx_bytes_total': 2048,
    })
    expect(rows.map((r) => r.label)).toEqual(['CPU', 'Memory', 'Network since boot'])
    expect(rows[0]?.value).toBe(formatPercent(2.5))
    expect(rows[1]?.value).toBe('2.00 GiB / 8.00 GiB (25.0%)')
    expect(rows[2]?.value).toBe('in 1.00 KiB / out 2.00 KiB')
  })

  test('empty snapshot produces no rows', () => {
    expect(buildStatusRows({})).toEqual([])
  })
})
