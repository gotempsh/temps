// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Skeleton } from '@temps-sdk/ui'
import { PageContainer, PageHeader } from '../page-header'
import { ResponsivePagination, type ResponsivePaginationProps } from '../responsive-pagination'
import { cn } from '../lib/cn'

export interface CardGridProps<T> {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  /** Search/filter row rendered between the header and the grid. */
  toolbar?: ReactNode
  items: T[]
  keyFn: (item: T) => string | number
  /** Renders one card per item — bring your own card component (e.g. `ProjectCard`). */
  renderCard: (item: T) => ReactNode
  isLoading?: boolean
  /** Number of skeleton cards to render while loading. */
  loadingCount?: number
  /** Rendered instead of the grid when `items` is empty and not loading — pass a `PageState`. */
  empty?: ReactNode
  pagination?: ResponsivePaginationProps
  className?: string
  /** Grid column classes. Defaults to the console's existing card-grid breakpoints. */
  gridClassName?: string
}

/**
 * The list template for record collections better shown as cards than a
 * table — same header shape as `Ledger` (title/description/actions/
 * toolbar), a responsive grid body instead of a `DataTable`. Does not touch
 * or reimplement any specific card component: pass the existing card (e.g.
 * `ProjectCard`) as `renderCard`. Only the grid shell — header, layout,
 * loading skeleton, empty state, pagination — is generic.
 */
export function CardGrid<T>({
  title,
  description,
  actions,
  toolbar,
  items,
  keyFn,
  renderCard,
  isLoading = false,
  loadingCount = 6,
  empty,
  pagination,
  className,
  gridClassName,
}: CardGridProps<T>) {
  const showEmpty = !isLoading && items.length === 0 && empty
  const grid = cn('grid gap-4 sm:grid-cols-2 xl:grid-cols-3', gridClassName)

  return (
    <PageContainer className={className}>
      <PageHeader title={title} description={description} actions={actions} />
      {toolbar}
      {showEmpty ? (
        empty
      ) : (
        <div className={grid} aria-busy={isLoading || undefined}>
          {isLoading
            ? Array.from({ length: loadingCount }).map((_, i) => (
                <div key={i} className="space-y-3 rounded-lg border p-4">
                  <Skeleton className="h-5 w-2/3" />
                  <Skeleton className="h-4 w-full" />
                  <Skeleton className="h-4 w-1/2" />
                </div>
              ))
            : items.map((item) => <div key={keyFn(item)}>{renderCard(item)}</div>)}
        </div>
      )}
      {pagination ? <ResponsivePagination {...pagination} /> : null}
    </PageContainer>
  )
}
