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
import { ResponsivePagination, type ResponsivePaginationProps } from '../responsive-pagination'
import { cn } from '../lib/cn'

export interface DataTableColumn<T> {
  key: string
  header: ReactNode
  render: (row: T) => ReactNode
  className?: string
}

export interface DataTableProps<T> {
  columns: DataTableColumn<T>[]
  rows: T[]
  rowKey: (row: T) => string | number
  onRowClick?: (row: T) => void
  /** Advanced rows own their cells, expansion and interactions; return table rows only. */
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
  onRowClick,
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
      <div className="rounded-md border">
        <Table aria-label={ariaLabel} aria-busy={isLoading}>
          <TableHeader>
            <TableRow>
              {columns.map((column) => (
                <TableHead key={column.key} className={column.className}>
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
                    <TableRow
                      key={rowKey(row)}
                      className={cn(onRowClick && 'cursor-pointer')}
                      onClick={onRowClick ? () => onRowClick(row) : undefined}
                    >
                      {columns.map((column) => (
                        <TableCell
                          key={column.key}
                          className={column.className}
                        >
                          {column.render(row)}
                        </TableCell>
                      ))}
                    </TableRow>
                  )
                )}
          </TableBody>
        </Table>
      </div>
      {pagination ? <ResponsivePagination {...pagination} /> : null}
    </div>
  )
}
