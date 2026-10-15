// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { dateFilterFromParams } from './useAnalyticsDateRange'

test('a known quick filter is kept and a missing one defaults to 24 hours', () => {
  expect(dateFilterFromParams('7days', null, null)).toEqual({
    quickFilter: '7days',
    dateRange: undefined,
  })
  expect(dateFilterFromParams(null, null, null).quickFilter).toBe('24hours')
  expect(dateFilterFromParams('qa-bogus', null, null).quickFilter).toBe(
    '24hours'
  )
})

test('a custom filter needs both ends and parses them', () => {
  const from = '2026-10-01T00:00:00.000Z'
  const to = '2026-10-02T00:00:00.000Z'
  const filter = dateFilterFromParams('custom', from, to)
  expect(filter.quickFilter).toBe('custom')
  expect(filter.dateRange?.from?.toISOString()).toBe(from)
  expect(filter.dateRange?.to?.toISOString()).toBe(to)
  // A bare `custom` (no range yet) is still a known quick filter.
  expect(dateFilterFromParams('custom', from, null)).toEqual({
    quickFilter: 'custom',
    dateRange: undefined,
  })
})
