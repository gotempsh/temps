// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { pageSparkline } from './page-sparkline'

test('single occupied hour keeps empty hours and UTC bucket positions', () => {
  const data = pageSparkline(
    [{ timestamp: '2026-09-10 07:00:00', session_count: 5 }],
    new Date('2026-09-10T06:00Z'),
    new Date('2026-09-10T08:00Z')
  )
  expect(data.map((p) => p.sessions)).toEqual([0, 5, 0])
  expect(data[1].time).toBe(Date.parse('2026-09-10T07:00Z'))
})
test('daily data is not interpreted as hourly data', () => {
  const data = pageSparkline(
    [{ timestamp: '2026-09-03T00:00Z', session_count: 4 }],
    new Date('2026-09-01T12:00Z'),
    new Date('2026-09-08T12:00Z')
  )
  expect(data).toHaveLength(8)
  expect(data.map((p) => p.sessions)).toEqual([0, 0, 4, 0, 0, 0, 0, 0])
})
test('monthly buckets use calendar boundaries across unequal month lengths', () => {
  const data = pageSparkline([], new Date('2026-01-15'), new Date('2026-09-10'))
  expect(data).toHaveLength(9)
  expect(data[2].time).toBe(Date.parse('2026-03-01T00:00Z'))
  expect(data.every((p) => p.sessions === 0)).toBe(true)
})
test('invalid and reversed ranges produce no chart', () => {
  expect(pageSparkline([], new Date('invalid'), new Date())).toEqual([])
  expect(
    pageSparkline([], new Date('2026-09-10'), new Date('2026-09-09'))
  ).toEqual([])
})
