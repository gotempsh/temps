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
      className={cn(align === 'right' && 'text-right', className)}
    >
      <button
        type="button"
        onClick={onClick}
        aria-label={`Sort by ${label}, ${nextDirection}`}
        className={cn(
          'inline-flex items-center gap-1 rounded py-2 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
          align === 'right' && 'ml-auto',
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
