// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parsePeriod } from './period.js'

describe('parsePeriod', () => {
  test('"today" starts at local midnight', () => {
    const { startDate, endDate, label } = parsePeriod('today')
    const start = new Date(startDate)
    expect(start.getHours()).toBe(0)
    expect(start.getMinutes()).toBe(0)
    expect(start.getSeconds()).toBe(0)
    expect(Date.parse(endDate)).toBeGreaterThanOrEqual(Date.parse(startDate))
    expect(label).toBe('today')
  })

  test('parses hour windows and singular-vs-plural labels', () => {
    const one = parsePeriod('1h')
    expect(one.label).toBe('last hour')
    const six = parsePeriod('6h')
    expect(six.label).toBe('last 6 hours')
    const startMs = Date.parse(six.startDate)
    const endMs = Date.parse(six.endDate)
    expect(endMs - startMs).toBeCloseTo(6 * 60 * 60 * 1000, -2)
  })

  test('parses day windows', () => {
    const week = parsePeriod('7d')
    expect(week.label).toBe('last 7 days')
    const startMs = Date.parse(week.startDate)
    const endMs = Date.parse(week.endDate)
    expect(endMs - startMs).toBeCloseTo(7 * 24 * 60 * 60 * 1000, -2)
  })

  test('parses month windows using calendar month subtraction', () => {
    const threeMonths = parsePeriod('3m')
    expect(threeMonths.label).toBe('last 3 months')
    const now = new Date()
    const expectedStart = new Date(now)
    expectedStart.setMonth(expectedStart.getMonth() - 3)
    expect(Date.parse(threeMonths.startDate)).toBeCloseTo(expectedStart.getTime(), -3)
  })

  test('singular "1d" and "1m" get non-plural labels', () => {
    expect(parsePeriod('1d').label).toBe('last day')
    expect(parsePeriod('1m').label).toBe('last month')
  })

  test('rejects a period with no valid unit suffix', () => {
    expect(() => parsePeriod('week')).toThrow(/Invalid period "week"/)
  })

  test('rejects an unsupported unit', () => {
    // "w" (weeks) is a plausible typo for "d" but was never a supported unit.
    expect(() => parsePeriod('2w')).toThrow(/Invalid period "2w"/)
  })

  test('rejects an empty string', () => {
    expect(() => parsePeriod('')).toThrow(/Invalid period/)
  })
})
