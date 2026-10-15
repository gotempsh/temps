// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SortDirection } from '@/components/ui/sortable-table-head'

export interface TableSort<Key extends string> {
  key: Key
  direction: SortDirection
}

export function nextTableSort<Key extends string>(
  current: TableSort<Key>,
  key: Key
): TableSort<Key> {
  if (current.key !== key) return { key, direction: 'desc' }
  return {
    key,
    direction: current.direction === 'desc' ? 'asc' : 'desc',
  }
}

export function sortTableRows<Row>(
  rows: readonly Row[],
  value: (row: Row) => string | number | null | undefined,
  direction: SortDirection
): Row[] {
  return rows
    .map((row, index) => ({ row, index }))
    .sort((a, b) => {
      const aValue = value(a.row)
      const bValue = value(b.row)

      if (aValue == null && bValue == null) return a.index - b.index
      if (aValue == null) return 1
      if (bValue == null) return -1

      const comparison =
        typeof aValue === 'string' && typeof bValue === 'string'
          ? aValue.localeCompare(bValue, undefined, {
              numeric: true,
              sensitivity: 'base',
            })
          : Number(aValue) - Number(bValue)
      const directed = direction === 'asc' ? comparison : -comparison
      if (directed !== 0) return directed
      // Rows that tie keep their incoming order when descending (the default
      // a column starts in) and the exact reverse when ascending. When every
      // value in a column is equal (all-zero bounce rates, one view each),
      // flipping the direction then still visibly reverses the list instead
      // of looking like the click did nothing.
      return direction === 'desc' ? a.index - b.index : b.index - a.index
    })
    .map(({ row }) => row)
}
