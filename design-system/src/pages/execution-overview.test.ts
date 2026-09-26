// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { executionOverview } from './execution-overview'
import { createExecutionFixtures } from '../fixtures'

const end = Date.parse('2026-09-18T12:00:00Z')
const records = createExecutionFixtures(end)

test('presets change the actual selected records, not only the label', () => {
  for (const [hours, count] of [
    [1, 4],
    [6, 24],
    [24, 96],
    [168, 672],
  ]) {
    const overview = executionOverview(
      records,
      new Date(end - hours * 3600000).toISOString(),
      new Date(end).toISOString(),
      '',
      'all',
    )
    assert.equal(overview.rows.length, count)
    assert.equal(
      overview.trend.reduce((sum, bin) => sum + bin.failed + bin.succeeded, 0),
      count,
    )
  }
})

test('search and result filters also govern metrics and the trend', () => {
  const overview = executionOverview(
    records,
    '2026-09-17T12:00:00Z',
    '2026-09-18T12:00:00Z',
    '  CLEANUP ',
    'failed',
  )
  assert.equal(overview.rows.length, 2)
  assert.equal(overview.failed, 2)
  assert.equal(overview.successRate, 0)
  assert.equal(overview.p95, 1200)
  assert.equal(
    overview.trend.reduce((sum, bin) => sum + bin.failed, 0),
    2,
  )
})

test('custom boundaries include exact endpoints and empty selections have no invented rate', () => {
  const selected = executionOverview(
    records,
    records[3].executedAt,
    records[0].executedAt,
    '',
    'all',
  )
  assert.equal(selected.rows.length, 4)
  const empty = executionOverview(
    records,
    '2020-01-01T00:00:00Z',
    '2020-01-02T00:00:00Z',
    '',
    'all',
  )
  assert.equal(empty.rows.length, 0)
  assert.equal(empty.p95, null)
  assert.equal(empty.successRate, null)
  assert.equal(empty.failed, 0)
})
