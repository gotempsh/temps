// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  customTimeRange,
  localDateTime,
  quickTimeRange,
} from './date-time-range'

describe('date/time range control', () => {
  test('quick ranges preserve their exact duration and end at now', () => {
    const now = Date.parse('2026-09-09T12:34:56.789Z')
    for (const [preset, hours] of [
      ['1h', 1],
      ['6h', 6],
      ['1d', 24],
      ['7d', 168],
    ] as const) {
      const value = quickTimeRange(preset, now)
      expect(Date.parse(value.to)).toBe(now)
      expect(Date.parse(value.to) - Date.parse(value.from)).toBe(
        hours * 3600000
      )
    }
  })
  test('converts local date/time to UTC without dropping the time', () => {
    const result = customTimeRange('2026-09-08T09:30', '2026-09-09T17:45', 30)
    expect('value' in result).toBe(true)
    if ('value' in result) {
      expect(result.value.preset).toBe('custom')
      expect(result.value.from).toBe(new Date(2026, 8, 8, 9, 30).toISOString())
      expect(localDateTime(result.value.to)).toBe('2026-09-09T17:45')
    }
  })
  test('requires valid, ordered dates and caps the backend window', () => {
    expect(customTimeRange('', '2026-09-09T17:45', 30)).toHaveProperty(
      'field',
      'from'
    )
    expect(
      customTimeRange('2026-02-30T09:30', '2026-09-09T17:45', 30)
    ).toHaveProperty('field', 'from')
    expect(
      customTimeRange('2026-09-09T17:45', '2026-09-09T17:45', 30)
    ).toHaveProperty('message', 'End time must be after start time.')
    expect(
      customTimeRange('2026-09-09T17:45', '2026-09-08T17:45', 30)
    ).toHaveProperty('field', 'to')
    expect(
      customTimeRange('2026-01-01T00:00', '2026-03-01T00:00', 30)
    ).toHaveProperty('message', 'Choose a time range of 30 days or less.')
  })
})
