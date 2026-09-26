// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { nextTableSort, sortTableRows } from './client-table-sort'

describe('client table sorting', () => {
  test('starts new columns descending and toggles active columns', () => {
    expect(
      nextTableSort({ key: 'requests', direction: 'desc' }, 'duration')
    ).toEqual({
      key: 'duration',
      direction: 'desc',
    })
    expect(
      nextTableSort({ key: 'duration', direction: 'desc' }, 'duration')
    ).toEqual({
      key: 'duration',
      direction: 'asc',
    })
  })

  test('sorts numeric and natural text values without mutating input', () => {
    const rows = [
      { name: 'Page 10', value: 2 },
      { name: 'Page 2', value: 8 },
    ]

    expect(
      sortTableRows(rows, (row) => row.name, 'asc').map((row) => row.name)
    ).toEqual(['Page 2', 'Page 10'])
    expect(
      sortTableRows(rows, (row) => row.value, 'desc').map((row) => row.value)
    ).toEqual([8, 2])
    expect(rows[0].name).toBe('Page 10')
  })

  test('keeps missing values last in both directions and preserves ties', () => {
    const rows = [
      { id: 'first', value: 4 as number | null },
      { id: 'missing', value: null },
      { id: 'second', value: 4 as number | null },
    ]

    expect(
      sortTableRows(rows, (row) => row.value, 'asc').map((row) => row.id)
    ).toEqual(['first', 'second', 'missing'])
    expect(
      sortTableRows(rows, (row) => row.value, 'desc').map((row) => row.id)
    ).toEqual(['first', 'second', 'missing'])
  })
})
