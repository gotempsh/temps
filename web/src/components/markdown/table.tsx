// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Components } from 'react-markdown'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'

/**
 * Table overrides for Markdown written by a model: the console's own table
 * (the shared `Table`, compact, in its bordered surface) instead of the
 * typography plugin's. A list the assistant writes then reads like every other
 * list in the console.
 *
 * `not-prose` keeps the typography plugin's table rules off it. The table
 * scrolls inside its own surface when its columns do not fit, so a wide answer
 * never widens the chat. Column alignment from the Markdown (`:--:`) arrives
 * as a `style` and is passed through.
 */
export const markdownTable: Pick<
  Components,
  'table' | 'thead' | 'tbody' | 'tr' | 'th' | 'td'
> = {
  table({ node: _node, className: _className, ...props }) {
    return (
      <div className="not-prose my-3 overflow-hidden rounded-md border">
        <Table density="compact" {...props} />
      </div>
    )
  },
  thead({ node: _node, ...props }) {
    return <TableHeader {...props} />
  },
  tbody({ node: _node, ...props }) {
    return <TableBody {...props} />
  },
  tr({ node: _node, ...props }) {
    return <TableRow {...props} />
  },
  th({ node: _node, ...props }) {
    return <TableHead {...props} />
  },
  td({ node: _node, ...props }) {
    return <TableCell {...props} />
  },
}
