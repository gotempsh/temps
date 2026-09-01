// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/**
 * Standard page shell — owns responsive horizontal padding and content width
 * in one place so pages don't hand-roll `container mx-auto max-w-*` wrappers.
 *
 * Width intents:
 * - `full`  (default) — spans the whole content area. Use for lists, tables,
 *   dashboards, and detail pages.
 * - `wide`  — capped at max-w-7xl so dashboards don't sprawl on ultrawide
 *   monitors, but still uses most of the width.
 * - `form`  — capped at max-w-3xl and centered. Use for create/edit forms
 *   where a narrow measure is easier to read and fill in.
 *
 * Change the padding or a width cap here once and every page follows.
 */
export type PageWidth = 'full' | 'wide' | 'form'

const WIDTH_CLASS: Record<PageWidth, string> = {
  full: 'w-full',
  wide: 'w-full max-w-7xl mx-auto',
  form: 'w-full max-w-3xl mx-auto',
}

export function PageContainer({
  width = 'full',
  className,
  innerClassName,
  children,
}: {
  width?: PageWidth
  /** Extra classes on the padded outer wrapper. */
  className?: string
  /** Extra classes on the width-constrained inner wrapper (e.g. spacing). */
  innerClassName?: string
  children: ReactNode
}) {
  return (
    <div className={cn('w-full px-4 py-6 sm:px-6 lg:px-8', className)}>
      <div className={cn(WIDTH_CLASS[width], 'min-w-0', innerClassName)}>
        {children}
      </div>
    </div>
  )
}
