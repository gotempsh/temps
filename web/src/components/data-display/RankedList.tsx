// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'

/** Readable identity first, then labelled facts. Keeps long paths inside narrow screens. */
export function RankedList({
  label,
  items,
}: {
  label: string
  items: Array<{
    id: string
    title: ReactNode
    subtitle?: ReactNode
    facts: Array<{ label: string; value: ReactNode }>
  }>
}) {
  return (
    <ol aria-label={label} className="divide-y rounded-lg border bg-card">
      {items.map((item) => (
        <li
          key={item.id}
          className="min-w-0 p-3 sm:flex sm:items-center sm:justify-between sm:gap-4"
        >
          <div className="min-w-0 flex-1">
            <div className="break-words text-sm font-medium [overflow-wrap:anywhere]">
              {item.title}
            </div>
            {item.subtitle && (
              <p className="mt-1 break-words text-xs text-muted-foreground">
                {item.subtitle}
              </p>
            )}
          </div>
          <dl className="mt-3 flex flex-wrap gap-x-5 gap-y-2 sm:mt-0 sm:shrink-0 sm:text-right">
            {item.facts.map((fact) => (
              <div key={fact.label} className="min-w-0">
                <dt className="text-xs text-muted-foreground">{fact.label}</dt>
                <dd className="text-sm font-medium tabular-nums">
                  {fact.value}
                </dd>
              </div>
            ))}
          </dl>
        </li>
      ))}
    </ol>
  )
}
