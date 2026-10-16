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

  test('keeps missing values last in both directions', () => {
    const rows = [
      { id: 'first', value: 4 as number | null },
      { id: 'missing', value: null },
      { id: 'second', value: 4 as number | null },
      { id: 'low', value: 1 as number | null },
    ]

    expect(
      sortTableRows(rows, (row) => row.value, 'asc').map((row) => row.id)
    ).toEqual(['low', 'second', 'first', 'missing'])
    expect(
      sortTableRows(rows, (row) => row.value, 'desc').map((row) => row.id)
    ).toEqual(['first', 'second', 'low', 'missing'])
  })

  test('ties follow the sort direction so a toggle always changes the order', () => {
    const rows = [
      { id: 'a', value: 0 },
      { id: 'b', value: 0 },
      { id: 'c', value: 0 },
    ]

    const ascending = sortTableRows(rows, (row) => row.value, 'asc').map(
      (row) => row.id
    )
    const descending = sortTableRows(rows, (row) => row.value, 'desc').map(
      (row) => row.id
    )

    expect(descending).toEqual(['a', 'b', 'c'])
    expect(ascending).toEqual(['c', 'b', 'a'])
  })

  test('missing values keep their original relative order in both directions', () => {
    const rows = [
      { id: 'x', value: null as number | null },
      { id: 'y', value: null as number | null },
      { id: 'z', value: 3 as number | null },
    ]

    expect(
      sortTableRows(rows, (row) => row.value, 'asc').map((row) => row.id)
    ).toEqual(['z', 'x', 'y'])
    expect(
      sortTableRows(rows, (row) => row.value, 'desc').map((row) => row.id)
    ).toEqual(['z', 'x', 'y'])
  })
})
