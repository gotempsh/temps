// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import { Check } from 'lucide-react'
import type { ReactNode } from 'react'

export interface Choice<T extends string> {
  id: T
  label: ReactNode
  detail?: ReactNode
  /** A line about this option for this box, good or bad. */
  note?: ReactNode
  badge?: ReactNode
  icon?: ReactNode
  disabled?: boolean
}

/**
 * One question's answers as cards: a radio group, so arrow keys and screen
 * readers treat it as one choice.
 */
export function ChoiceList<T extends string>({
  label,
  choices,
  value,
  onChange,
  columns = 1,
  size = 'default',
}: {
  label: string
  choices: Choice<T>[]
  value: T | null
  onChange: (value: T) => void
  columns?: 1 | 2
  /**
   * `compact`: one line per choice (icon and label), for short pickers such
   * as an access level. The border alone marks the selection.
   */
  size?: 'default' | 'compact'
}) {
  const compact = size === 'compact'
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={cn(
        'grid grid-cols-1 gap-3',
        columns === 2 && 'md:grid-cols-2',
        compact && 'gap-2'
      )}
    >
      {choices.map((choice) => {
        // A choice that can't be picked never shows as picked.
        const selected = value === choice.id && !choice.disabled
        return (
          <button
            key={choice.id}
            type="button"
            role="radio"
            aria-checked={selected}
            aria-disabled={choice.disabled || undefined}
            onClick={() => !choice.disabled && onChange(choice.id)}
            className={cn(
              'flex min-w-0 rounded-lg border bg-card text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              compact
                ? 'items-center gap-2 px-3 py-2 text-sm'
                : 'items-start gap-3 p-4',
              selected
                ? 'border-primary ring-1 ring-primary'
                : 'hover:bg-muted/40',
              choice.disabled && 'cursor-not-allowed opacity-60 hover:bg-card'
            )}
          >
            {choice.icon ? (
              <span
                className={cn('shrink-0', !compact && 'mt-0.5')}
                aria-hidden
              >
                {choice.icon}
              </span>
            ) : null}
            <span className="min-w-0 flex-1">
              <span className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{choice.label}</span>
                {choice.badge}
              </span>
              {choice.detail ? (
                <span className="mt-1 block text-sm text-muted-foreground">
                  {choice.detail}
                </span>
              ) : null}
              {choice.note ? (
                <span className="mt-2 block text-xs text-muted-foreground [overflow-wrap:anywhere]">
                  {choice.note}
                </span>
              ) : null}
            </span>
            <span
              aria-hidden
              className={cn(
                compact && 'hidden',
                'mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border',
                selected && 'border-primary bg-primary text-primary-foreground'
              )}
            >
              {selected ? <Check className="size-3" /> : null}
            </span>
          </button>
        )
      })}
    </div>
  )
}
