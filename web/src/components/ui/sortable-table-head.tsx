// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ArrowDown, ArrowUp, ArrowUpDown } from 'lucide-react'

import { cn } from '@/lib/utils'
import { TableHead } from '@/components/ui/table'

export type SortDirection = 'asc' | 'desc'

interface SortableTableHeadProps {
  label: string
  active: boolean
  direction: SortDirection
  onClick: () => void
  align?: 'left' | 'right'
  className?: string
}

export function SortableTableHead({
  label,
  active,
  direction,
  onClick,
  align = 'left',
  className,
}: SortableTableHeadProps) {
  const nextDirection =
    active && direction === 'desc' ? 'ascending' : 'descending'

  return (
    <TableHead
      scope="col"
      aria-sort={
        active ? (direction === 'asc' ? 'ascending' : 'descending') : undefined
      }
      // The button fills the cell (padding moves from the cell to the
      // button) so the whole header is the click target, not just its label.
      className={cn('p-0', align === 'right' && 'text-right', className)}
    >
      <button
        type="button"
        onClick={onClick}
        aria-label={`Sort by ${label}, ${nextDirection}`}
        className={cn(
          'flex h-full min-h-12 w-full items-center gap-1 rounded px-4 py-2 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
          align === 'right' && 'justify-end',
          active && 'text-foreground'
        )}
      >
        {label}
        {active ? (
          direction === 'asc' ? (
            <ArrowUp className="size-3.5" aria-hidden="true" />
          ) : (
            <ArrowDown className="size-3.5" aria-hidden="true" />
          )
        ) : (
          <ArrowUpDown className="size-3.5" aria-hidden="true" />
        )}
      </button>
    </TableHead>
  )
}
