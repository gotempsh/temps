// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { sortTableRows } from '@/lib/client-table-sort'
import { durationSortValue } from './PageFlow'

test('entry and exit durations sort missing and nonpositive values together', () => {
  const rows = [
    { id: 'missing', duration: null },
    { id: 'long', duration: 12 },
    { id: 'zero', duration: 0 },
    { id: 'short', duration: 3 },
    { id: 'negative', duration: -2 },
  ]

  expect(
    sortTableRows(rows, (row) => durationSortValue(row.duration), 'asc').map(
      (row) => row.id
    )
  ).toEqual(['short', 'long', 'missing', 'zero', 'negative'])
  expect(
    sortTableRows(rows, (row) => durationSortValue(row.duration), 'desc').map(
      (row) => row.id
    )
  ).toEqual(['long', 'short', 'missing', 'zero', 'negative'])
})
