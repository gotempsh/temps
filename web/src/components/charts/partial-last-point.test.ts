// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { hourInProgress } from '@/lib/analytics-timeline'
import {
  partialTooltipEntries,
  withPartialLastPoint,
} from './partial-last-point'

const HOUR = 3_600_000

test('the solid line stops a point early and a dashed segment reaches the partial one', () => {
  const rows: { t: number; count: number | null }[] = [
    { t: 1, count: 40 },
    { t: 2, count: 60 },
    { t: 3, count: 9 },
  ]
  expect(withPartialLastPoint(rows, 'count')).toEqual([
    { t: 1, count: 40 },
    { t: 2, count: 60, __partial: 60 },
    { t: 3, count: null, __partial: 9 },
  ])
  // The caller's rows keep their values, so axes still fit the real data.
  expect(rows[2].count).toBe(9)
})

test('each point has one tooltip entry', () => {
  // Where the two segments meet, the solid value is the one shown.
  expect(
    partialTooltipEntries([
      { dataKey: 'count', value: 60 },
      { dataKey: '__partial', value: 60 },
    ])
  ).toEqual([{ dataKey: 'count', value: 60 }])
  // At the partial point only the dashed segment has a value.
  expect(
    partialTooltipEntries([
      { dataKey: 'count', value: null },
      { dataKey: '__partial', value: 9 },
    ])
  ).toEqual([{ dataKey: '__partial', value: 9 }])
})

test('only the hour still under way counts as partial', () => {
  const now = Date.UTC(2026, 9, 29, 7, 15)
  const thisHour = Date.UTC(2026, 9, 29, 7)
  expect(
    hourInProgress(
      [{ timestamp: thisHour - HOUR }, { timestamp: thisHour }],
      now
    )
  ).toBe(true)
  expect(hourInProgress([{ timestamp: thisHour - HOUR }], now)).toBe(false)
  expect(hourInProgress([], now)).toBe(false)
})
