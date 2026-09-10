// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { resolveTimeRange, serializeTimeRange } from './time-range-filter'
import { computeTracesTimeWindow } from './traces-time-window'

describe('existing page time range adapter', () => {
  test('keeps custom timestamps intact through URL persistence and refresh', () => {
    const value = {
      preset: 'custom' as const,
      from: '2026-09-07T09:30:00.000Z',
      to: '2026-09-08T17:45:00.000Z',
    }
    const params = new URLSearchParams({ range: serializeTimeRange(value) })
    const restored = new URLSearchParams(params.toString()).get('range')!
    expect(resolveTimeRange(restored)).toEqual(value)
    expect(computeTracesTimeWindow(restored, new Date('2026-10-01'))).toEqual({
      startTime: value.from,
      endTime: value.to,
    })
  })
  test('supports legacy links and exact quick durations across DST', () => {
    const now = Date.parse('2026-03-29T12:00:00Z')
    for (const [range, hours] of [
      ['1h', 1],
      ['6h', 6],
      ['24h', 24],
      ['1d', 24],
      ['7d', 168],
      ['30d', 720],
    ] as const) {
      const value = resolveTimeRange(range, now)
      expect(Date.parse(value.to) - Date.parse(value.from)).toBe(
        hours * 3600000
      )
    }
  })
  test('invalid custom links fall back to a valid day window', () => {
    const now = Date.now()
    for (const range of [
      'custom:no/no',
      'custom:2026-09-08/2026-09-07',
      'nonsense',
      '0h',
      '999999999999999999999999999999999d',
    ]) {
      const value = resolveTimeRange(range, now)
      expect(Date.parse(value.to) - Date.parse(value.from)).toBe(86400000)
    }
  })
})
