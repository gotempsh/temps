// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { analyticsTimeline } from './analytics-timeline'

test('fills empty UTC hours across the selected window without changing totals', () => {
  const result = analyticsTimeline(
    [{ date: '2026-09-10T11:00:00Z', count: 3 }],
    new Date('2026-09-10T09:00:00Z'),
    new Date('2026-09-10T13:00:00Z')
  )
  expect(result.map((row) => row.count)).toEqual([0, 0, 3, 0, 0])
  expect(result[2].timestamp).toBe(Date.parse('2026-09-10T11:00:00Z'))
})
test('treats timezone-less project buckets as UTC and sorts out-of-order data', () => {
  const result = analyticsTimeline(
    [
      { date: '2026-09-10 11:00', count: 2 },
      { date: '2026-09-10T10:00:00Z', count: 1 },
    ],
    new Date('2026-09-10T10:00:00Z'),
    new Date('2026-09-10T12:00:00Z')
  )
  expect(result.map((row) => row.count)).toEqual([1, 2, 0])
})
test('rejects invalid ranges and ignores invalid points', () => {
  expect(analyticsTimeline([], new Date('invalid'), new Date())).toEqual([])
  expect(
    analyticsTimeline(
      [{ date: 'invalid', count: 5 }],
      new Date('2026-09-10T10:00:00Z'),
      new Date('2026-09-10T10:30:00Z')
    )
  ).toEqual([{ timestamp: Date.parse('2026-09-10T10:00:00Z'), count: 0 }])
})
