// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'

export interface SummaryMetric {
  label: string
  value: number | string
  hint?: ReactNode
}

/** Compact labelled values; exact counts remain available when visually abbreviated. */
export function MetricSummary({
  metrics,
  loading = false,
}: {
  metrics: SummaryMetric[]
  loading?: boolean
}) {
  return (
    <dl
      aria-label="Analytics summary"
      aria-busy={loading}
      className="grid grid-cols-3 divide-x rounded-lg border bg-card"
    >
      {metrics.map(({ label, value, hint }) => (
        <div key={label} className="min-w-0 p-3 sm:p-4">
          <dt className="text-xs text-muted-foreground sm:text-sm">{label}</dt>
          <dd className="mt-1 text-xl font-semibold tabular-nums sm:text-2xl">
            {loading ? (
              <span
                className="block h-7 w-12 animate-pulse rounded bg-muted"
                aria-label="Loading"
              />
            ) : (
              <span
                title={
                  typeof value === 'number' ? value.toLocaleString() : value
                }
              >
                <span className="sm:hidden" aria-hidden="true">
                  {typeof value === 'number'
                    ? new Intl.NumberFormat(undefined, {
                        notation: 'compact',
                        maximumFractionDigits: 1,
                      }).format(value)
                    : value}
                </span>
                <span className="sr-only sm:not-sr-only">
                  {typeof value === 'number' ? value.toLocaleString() : value}
                </span>
              </span>
            )}
          </dd>
          {hint && !loading && (
            <dd className="mt-1 break-words text-xs text-muted-foreground">
              {hint}
            </dd>
          )}
        </div>
      ))}
    </dl>
  )
}
