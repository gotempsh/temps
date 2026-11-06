// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Fragment, type ReactNode } from 'react'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Skeleton,
} from '@temps-sdk/ui'
import {
  ResponsivePagination,
  type ResponsivePaginationProps,
} from '../responsive-pagination'
import { cn } from '../lib/cn'
import { RecordRow } from '../record-link'

export interface DataTableColumn<T> {
  key: string
  header: ReactNode
  render: (row: T) => ReactNode
  className?: string
  ariaSort?: 'ascending' | 'descending' | 'none'
}

export interface DataTableProps<T> {
  columns: DataTableColumn<T>[]
  rows: T[]
  rowKey: (row: T) => string | number
  /**
   * Advanced rows own their cells, expansion and interactions; return table
   * rows only, as a `RecordRow` when the row has a `RecordLink`.
   */
  renderRow?: (row: T) => ReactNode
  isLoading?: boolean
  /** Accessible name when the surrounding heading does not label the table. */
  'aria-label'?: string
  pagination?: ResponsivePaginationProps
  className?: string
}

/**
 * The table + loading-skeleton + pagination-footer body that `Ledger`
 * composes. Extracted so embedded (non-full-page) tables — a settings
 * sub-panel, a `Detail`'s `main` column — can reuse the same table
 * rendering without inheriting `Ledger`'s `PageContainer`/`PageHeader`.
 * Callers own their own empty state (render it instead of `DataTable`, the
 * way `Ledger` does with its `empty` prop) since "no rows" copy is always
 * context-specific.
 */
export function DataTable<T>({
  columns,
  rows,
  rowKey,
  renderRow,
  isLoading = false,
  'aria-label': ariaLabel,
  pagination,
  className,
}: DataTableProps<T>) {
  return (
    <div className={cn('space-y-6', className)}>
      {isLoading ? (
        <p role="status" className="sr-only">
          Loading rows…
        </p>
      ) : null}
      {/* A query container: columns use `hidden @xl:table-cell` and show by
          the width this table has, sidebar and side panels included. */}
      <div className="@container rounded-md border">
        <Table aria-label={ariaLabel} aria-busy={isLoading}>
          <TableHeader>
            <TableRow>
              {columns.map((column) => (
                <TableHead
                  key={column.key}
                  className={column.className}
                  aria-sort={column.ariaSort}
                >
                  {column.header}
                </TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {isLoading
              ? Array.from({ length: 5 }).map((_, i) => (
                  <TableRow key={i}>
                    {columns.map((column) => (
                      <TableCell key={column.key} className={column.className}>
                        <Skeleton className="h-4 w-24" />
                      </TableCell>
                    ))}
                  </TableRow>
                ))
              : rows.map((row) =>
                  renderRow ? (
                    <Fragment key={rowKey(row)}>{renderRow(row)}</Fragment>
                  ) : (
                    <RecordRow key={rowKey(row)}>
                      {columns.map((column) => (
                        <TableCell
                          key={column.key}
                          className={column.className}
                        >
                          {column.render(row)}
                        </TableCell>
                      ))}
                    </RecordRow>
                  )
                )}
          </TableBody>
        </Table>
      </div>
      {pagination ? <ResponsivePagination {...pagination} /> : null}
    </div>
  )
}
