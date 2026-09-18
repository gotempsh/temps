// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import type { ResponsivePaginationProps } from '../responsive-pagination'
import { PageContainer, PageHeader } from '../page-header'
import { DataTable, type DataTableColumn } from './data-table'

export type LedgerColumn<T> = DataTableColumn<T>

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
  pagination?: ResponsivePaginationProps
  className?: string
}

/**
 * The list template: header, optional toolbar, a table, optional pagination.
 * Pairs with `useUrlState` for the toolbar's filters and `pagination.page`
 * so a filtered, paginated list survives a refresh or a shared link — see
 * RULES.md § "the URL is the state".
 *
 * The table itself is `DataTable` — `Ledger` only adds the full-page shell
 * (`PageContainer`/`PageHeader`) and the empty-state branch around it. Reach
 * for `DataTable` directly for an embedded table (a settings sub-panel, a
 * `Detail`'s `main` column) that doesn't want a second page header.
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
        <DataTable
          columns={columns}
          rows={rows}
          rowKey={rowKey}
          onRowClick={onRowClick}
          isLoading={isLoading}
          pagination={pagination}
        />
      )}
    </PageContainer>
  )
}
