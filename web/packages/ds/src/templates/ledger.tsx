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
import { PageContainer, PageHeader } from '../page-header'
import { Button } from '../button'
import { cn } from '../lib/cn'

export interface LedgerColumn<T> {
  key: string
  header: ReactNode
  render: (row: T) => ReactNode
  className?: string
}

export interface LedgerProps<T> {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  /** Search/filter row rendered between the header and the table. */
  toolbar?: ReactNode
  columns: LedgerColumn<T>[]
  rows: T[]
  rowKey: (row: T) => string | number
  onRowClick?: (row: T) => void
  isLoading?: boolean
  /** Rendered instead of the table when `rows` is empty and not loading — pass a `PageState`. */
  empty?: ReactNode
  pagination?: {
    page: number
    pageCount: number
    onPageChange: (page: number) => void
  }
  className?: string
}

/**
 * The list template: header, optional toolbar, a table, optional pagination.
 * Pairs with `useUrlState` for the toolbar's filters and `pagination.page`
 * so a filtered, paginated list survives a refresh or a shared link — see
 * RULES.md § "the URL is the state".
 */
export function Ledger<T>({
  title,
  description,
  actions,
  toolbar,
  columns,
  rows,
  rowKey,
  onRowClick,
  isLoading = false,
  empty,
  pagination,
  className,
}: LedgerProps<T>) {
  const showEmpty = !isLoading && rows.length === 0 && empty

  return (
    <PageContainer className={className}>
      <PageHeader title={title} description={description} actions={actions} />
      {toolbar}
      {showEmpty ? (
        empty
      ) : (
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
      )}
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
    </PageContainer>
  )
}
