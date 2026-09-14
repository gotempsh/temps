// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/**
 * Standard full-width page shell. It owns responsive horizontal padding so
 * lists, detail pages, and forms use the same available content width.
 */
export function PageContainer({
  className,
  innerClassName,
  children,
}: {
  /** Extra classes on the padded outer wrapper. */
  className?: string
  /** Extra classes on the content wrapper (e.g. spacing). */
  innerClassName?: string
  children: ReactNode
}) {
  return (
    <div
      data-page-container
      className={cn('w-full px-4 py-6 sm:px-6 lg:px-8', className)}
    >
      <div
        className={cn('w-full min-w-0 space-y-6', innerClassName)}
      >
        {children}
      </div>
    </div>
  )
}

/**
 * Standard heading block for top-level console pages.
 *
 * Actions stack below the title on narrow screens and align to the right once
 * there is enough room. Keeping the visible page title here also guarantees a
 * single h1 with consistent typography across platform sections.
 */
export function PageHeader({
  title,
  description,
  actions,
  className,
}: {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  className?: string
}) {
  return (
    <div
      data-page-header
      className={cn(
        'flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between',
        className
      )}
    >
      <div className="min-w-0">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
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
