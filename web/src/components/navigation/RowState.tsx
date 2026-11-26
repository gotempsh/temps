// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TreeState } from '@/lib/entity-tree'
import { cn } from '@/lib/utils'

const TROUBLE_TEXT = {
  warn: 'text-amber-700 dark:text-amber-400',
  bad: 'text-red-700 dark:text-red-400',
} as const

/**
 * A row's state. Working and idle are a quiet dot, filled or hollow, so the
 * name keeps the row; trouble is said in a word, read and not guessed from
 * a colour.
 */
export function RowState({ state }: { state: TreeState | undefined }) {
  if (!state) return null
  if (state.tone === 'ok' || state.tone === 'idle')
    return (
      <span
        title={state.label}
        className={cn(
          'ml-auto size-1.5 shrink-0 rounded-full',
          state.tone === 'ok'
            ? 'bg-emerald-500'
            : 'border border-muted-foreground/60'
        )}
      >
        <span className="sr-only">{state.label}</span>
      </span>
    )
  return (
    <span
      className={cn(
        'ml-auto shrink-0 text-xs font-medium',
        TROUBLE_TEXT[state.tone]
      )}
    >
      {state.label}
    </span>
  )
}
