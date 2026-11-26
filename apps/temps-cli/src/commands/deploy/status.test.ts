// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { calculateDuration } from './status.js'

/** The ISO 8601 instant `seconds` after the epoch. */
const at = (seconds: number) => new Date(seconds * 1000).toISOString()

describe('calculateDuration', () => {
  test('reads fractional seconds as part of the instant', () => {
    expect(calculateDuration('2026-01-01T00:00:00.500Z', '2026-01-01T00:00:31Z')).toBe('30s')
  })

  test('renders sub-minute durations as seconds only', () => {
    expect(calculateDuration(at(0), at(30))).toBe('30s')
  })

  test('renders minutes and remaining seconds once over a minute', () => {
    expect(calculateDuration(at(0), at(90))).toBe('1m 30s')
  })

  test('renders an exact minute boundary with 0 remaining seconds', () => {
    expect(calculateDuration(at(0), at(60))).toBe('1m 0s')
  })

  test('has no hour tier — durations over an hour still render as minutes', () => {
    // 3661s = 1h 1m 1s, but the formatter caps out at minutes.
    expect(calculateDuration(at(0), at(3661))).toBe('61m 1s')
  })
})
