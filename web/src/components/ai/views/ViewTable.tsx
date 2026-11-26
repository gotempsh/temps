// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Status } from '@temps-sdk/ds'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { cn } from '@/lib/utils'
import { ConsoleLink } from './ConsoleLink'
import { isFigureColumn, type ViewCell } from './view-table'

function Cell({ cell }: { cell: ViewCell | undefined }) {
  if (cell === undefined || typeof cell === 'string') return <>{cell ?? ''}</>
  if (cell.tone) {
    return <Status variant="dot" tone={cell.tone} label={cell.text} />
  }
  return <ConsoleLink href={cell.href}>{cell.text}</ConsoleLink>
}

/**
 * Records the assistant listed. It measures the chat column, not the window:
 * where there is room it is the console's compact table, and where there is
 * not (a phone, the narrow dock) each record is its first cell with the rest
 * as labelled lines under it, so nothing scrolls sideways.
 */
export function ViewTable({
  columns,
  rows,
}: {
  columns: string[]
  rows: ViewCell[][]
}) {
  // Two columns fit anywhere; more need the room.
  const stacks = columns.length > 2
  const figures = columns.map((_, index) => isFigureColumn(rows, index))
  return (
    <div className="@container">
      {stacks ? (
        <ul className="divide-y rounded-md border text-sm @lg:hidden">
          {rows.map((row, rowIndex) => (
            <li key={rowIndex} className="space-y-1 px-3 py-2">
              <p className="font-medium [overflow-wrap:anywhere]">
                <Cell cell={row[0]} />
              </p>
              <dl className="space-y-0.5">
                {columns.slice(1).map((column, index) => (
                  <div
                    key={`${index}:${column}`}
                    className="flex min-w-0 items-baseline justify-between gap-4"
                  >
                    <dt className="shrink-0 text-xs text-muted-foreground">
                      {column}
                    </dt>
                    <dd
                      className={cn(
                        'min-w-0 text-right [overflow-wrap:anywhere]',
                        figures[index + 1] && 'tabular-nums'
                      )}
                    >
                      <Cell cell={row[index + 1]} />
                    </dd>
                  </div>
                ))}
              </dl>
            </li>
          ))}
        </ul>
      ) : null}
      <div
        className={cn(
          'overflow-hidden rounded-md border',
          stacks && 'hidden @lg:block'
        )}
      >
        <Table density="compact">
          <TableHeader>
            <TableRow>
              {columns.map((column, index) => (
                <TableHead
                  key={`${index}:${column}`}
                  className={cn(figures[index] && 'text-right')}
                >
                  {column}
                </TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row, rowIndex) => (
              <TableRow key={rowIndex}>
                {columns.map((_, columnIndex) => (
                  <TableCell
                    key={columnIndex}
                    className={cn(
                      '[overflow-wrap:anywhere]',
                      figures[columnIndex] && 'text-right tabular-nums'
                    )}
                  >
                    <Cell cell={row[columnIndex]} />
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  )
}
