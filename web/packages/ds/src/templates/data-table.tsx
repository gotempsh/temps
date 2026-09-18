// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Skeleton,
} from '@temps-sdk/ui'
import { Button } from '../button'
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
  isLoading?: boolean
  pagination?: {
    page: number
    pageCount: number
    onPageChange: (page: number) => void
  }
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
  isLoading = false,
  pagination,
  className,
}: DataTableProps<T>) {
  return (
    <div className={cn('space-y-6', className)}>
      <div className="rounded-md border">
        <Table>
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
                      <TableCell key={column.key}>
                        <Skeleton className="h-4 w-24" />
                      </TableCell>
                    ))}
                  </TableRow>
                ))
              : rows.map((row) => (
                  <TableRow
                    key={rowKey(row)}
                    className={cn(onRowClick && 'cursor-pointer')}
                    onClick={() => onRowClick?.(row)}
                  >
                    {columns.map((column) => (
                      <TableCell key={column.key} className={column.className}>
                        {column.render(row)}
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
          </TableBody>
        </Table>
      </div>
      {pagination && pagination.pageCount > 1 ? (
        <div className="flex items-center justify-between text-sm text-muted-foreground">
          <span>
            Page {pagination.page} of {pagination.pageCount}
          </span>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="sm"
              aria-disabled={pagination.page <= 1}
              className={cn(pagination.page <= 1 && 'pointer-events-none opacity-50')}
              onClick={() => pagination.onPageChange(pagination.page - 1)}
            >
              Previous
            </Button>
            <Button
              variant="outline"
              size="sm"
              aria-disabled={pagination.page >= pagination.pageCount}
              className={cn(
                pagination.page >= pagination.pageCount && 'pointer-events-none opacity-50',
              )}
              onClick={() => pagination.onPageChange(pagination.page + 1)}
            >
              Next
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  )
}
