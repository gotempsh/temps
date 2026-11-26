// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ConsoleLink } from './ConsoleLink'

export interface ViewBarItem {
  label: string
  value: number
  text?: string | null
  href?: string | null
}

/**
 * One amount compared across named things: a name, a bar and a figure per
 * row, every bar on the same baseline and scale so lengths compare exactly.
 * One hue, because the rows are one measure, not different series; the name
 * and the figure are text, never the bar's colour. In a narrow chat the bar
 * drops under the name instead of squeezing it.
 */
export function ViewBars({ items }: { items: ViewBarItem[] }) {
  const largest = Math.max(...items.map((item) => item.value), 0)
  return (
    <ul className="@container space-y-2.5 rounded-md border p-3 text-sm">
      {items.map((item, index) => (
        <li
          key={`${index}:${item.label}`}
          className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 @md:grid-cols-[minmax(0,12rem)_minmax(0,1fr)_auto]"
        >
          <span className="min-w-0 [overflow-wrap:anywhere]">
            <ConsoleLink href={item.href}>{item.label}</ConsoleLink>
          </span>
          <span
            aria-hidden="true"
            className="col-span-2 row-start-2 block @md:col-span-1 @md:col-start-2 @md:row-start-1"
          >
            {/* Square at the baseline, rounded at the data end; a zero draws
                nothing, and the smallest amount is still a visible mark. */}
            {item.value > 0 && largest > 0 ? (
              <span
                className="block h-3 min-w-0.5 rounded-r-[4px] bg-[var(--chart-1)]"
                style={{ width: `${(item.value / largest) * 100}%` }}
              />
            ) : null}
          </span>
          <span className="col-start-2 row-start-1 text-right tabular-nums @md:col-start-3">
            {item.text ?? item.value.toLocaleString()}
          </span>
        </li>
      ))}
    </ul>
  )
}
