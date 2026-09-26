// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Skeleton } from '@/components/ui/skeleton'

export function CheckLoading({
  label = 'Loading checks…',
}: {
  label?: string
}) {
  return (
    <div role="status" aria-label={label} className="divide-y">
      <span className="sr-only">{label}</span>
      <div
        aria-hidden="true"
        className="grid grid-cols-4 gap-4 bg-muted/40 p-4"
      >
        {Array.from({ length: 4 }, (_, i) => (
          <Skeleton key={i} className="h-4 w-16" />
        ))}
      </div>
      {Array.from({ length: 3 }, (_, row) => (
        <div
          aria-hidden="true"
          key={row}
          className="grid grid-cols-4 gap-4 p-4"
        >
          {Array.from({ length: 4 }, (_, col) => (
            <Skeleton key={col} className="h-5 w-full" />
          ))}
        </div>
      ))}
    </div>
  )
}
