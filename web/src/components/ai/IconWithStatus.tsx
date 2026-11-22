// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/** How a toolbar button's state reads at a glance. */
export type IconStatusTone = 'good' | 'working' | 'attention' | 'bad'

const DOT_CLASSES: Record<IconStatusTone, string> = {
  good: 'bg-emerald-500',
  working: 'bg-sky-500 animate-pulse',
  attention: 'bg-amber-500',
  bad: 'bg-destructive',
}

/**
 * A toolbar icon that keeps saying what the button is, with its state as a
 * small dot on the corner. Replacing the icon with a bare check or cross
 * leaves a row of status marks nobody can tell apart.
 */
export function IconWithStatus({
  icon,
  tone,
}: {
  icon: ReactNode
  /** No dot when the button has nothing to report. */
  tone: IconStatusTone | null
}) {
  return (
    <span className="relative inline-flex">
      {icon}
      {tone && (
        <span
          data-status={tone}
          aria-hidden
          className={cn(
            'absolute -right-0.5 -top-0.5 size-2 rounded-full ring-2 ring-background',
            DOT_CLASSES[tone]
          )}
        />
      )}
    </span>
  )
}
