// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from './lib/cn'

// Promoted from web/src/components/layout/PageContainer.tsx (it was already
// solid — full-width shell, responsive padding). web/src's own
// PageContainer.tsx now re-exports this module instead of defining its own
// copy, so there is exactly one implementation. `PageHeader` is extended
// here with the optional `verdict` slot the record recipe needs (title ->
// verdict -> facts).

export interface PageContainerProps {
  /** Extra classes on the padded outer wrapper. */
  className?: string
  /** Extra classes on the content wrapper (e.g. spacing). */
  innerClassName?: string
  children: ReactNode
}

/** Standard full-width page shell — owns responsive horizontal padding. */
export function PageContainer({ className, innerClassName, children }: PageContainerProps) {
  return (
    <div data-page-container className={cn('w-full px-4 py-6 sm:px-6 lg:px-8', className)}>
      <div className={cn('w-full min-w-0 space-y-6', innerClassName)}>{children}</div>
    </div>
  )
}

export interface PageHeaderProps {
  title: ReactNode
  description?: ReactNode
  /**
   * The record recipe's second beat: a `Status` or similar one-glance
   * verdict, rendered directly under the title before any facts. Omit on
   * list/settings pages — only record (`Detail`) pages have a verdict.
   */
  verdict?: ReactNode
  actions?: ReactNode
  className?: string
}

export function PageHeader({
  title,
  description,
  verdict,
  actions,
  className,
}: PageHeaderProps) {
  return (
    <div
      data-page-header
      className={cn(
        'flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between',
        className,
      )}
    >
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
          {verdict}
        </div>
        {description ? (
          <p className="mt-1 text-sm text-muted-foreground">{description}</p>
        ) : null}
      </div>
      {actions ? (
        <div className="flex min-w-0 flex-wrap items-center gap-2 sm:justify-end">
          {actions}
        </div>
      ) : null}
    </div>
  )
}
