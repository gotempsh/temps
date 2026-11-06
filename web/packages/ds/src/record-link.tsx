// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ComponentProps, MouseEvent } from 'react'
import { Link, type LinkProps } from 'react-router'
import { TableRow } from '@temps-sdk/ui'
import { cn } from './lib/cn'
import { followsRecordLink } from './lib/follows-record-link'

/**
 * Canonical resource-detail navigation in a table's identity column. It is
 * a real router link (keyboard, new tab, copy link) styled as the record's
 * name; the surrounding `RecordRow` makes the whole row follow it.
 */
export function RecordLink({ children, className, ...props }: LinkProps) {
  return (
    <Link
      {...props}
      data-record-link=""
      className={cn(
        'inline-flex min-h-9 max-w-full items-center rounded-sm text-sm font-medium text-foreground no-underline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-4',
        className
      )}
    >
      <span className="min-w-0 break-all">{children}</span>
    </Link>
  )
}

type FollowHandlers<E extends HTMLElement> = {
  onClick?: (event: MouseEvent<E>) => void
  onAuxClick?: (event: MouseEvent<E>) => void
}

/**
 * Click handlers that make an element follow the one `RecordLink` it
 * contains. Cmd/Ctrl-click and middle click open a new tab; clicks on the
 * element's own controls, in portals they open, or that select text never
 * navigate. With no record link, or more than one, a click does nothing:
 * the row cannot know which record is meant.
 */
function followHandlers<E extends HTMLElement>({
  onClick,
  onAuxClick,
}: FollowHandlers<E>): Required<FollowHandlers<E>> {
  const follow = (event: MouseEvent<E>, newTab: boolean) => {
    const item = event.currentTarget
    const links = item.querySelectorAll<HTMLAnchorElement>(
      'a[data-record-link]'
    )
    if (links.length !== 1) return
    if (!followsRecordLink(item, event.target as Element)) return
    if (window.getSelection()?.toString()) return
    if (newTab) window.open(links[0].href, '_blank', 'noopener')
    else links[0].click()
  }
  return {
    onClick: (event) => {
      onClick?.(event)
      if (!event.defaultPrevented) follow(event, event.metaKey || event.ctrlKey)
    },
    onAuxClick: (event) => {
      onAuxClick?.(event)
      if (!event.defaultPrevented && event.button === 1) follow(event, true)
    },
  }
}

/** Shows the pointer only on items that hold a record link. */
const FOLLOWS = 'has-[a[data-record-link]]:cursor-pointer'

/**
 * A table row that opens its record when clicked anywhere outside its own
 * controls, by following the `RecordLink` it contains. Keyboard users reach
 * the record through the link itself, so the row keeps its table semantics
 * (no `tabIndex`, no role). A row without a `RecordLink` behaves like
 * `TableRow`.
 */
export function RecordRow({
  className,
  onClick,
  onAuxClick,
  ...props
}: ComponentProps<typeof TableRow>) {
  return (
    <TableRow
      {...props}
      className={cn(FOLLOWS, className)}
      {...followHandlers({ onClick, onAuxClick })}
    />
  )
}

/** `RecordRow` for a `<ul role="list">` of records instead of a table. */
export function RecordListItem({
  className,
  onClick,
  onAuxClick,
  ...props
}: ComponentProps<'li'>) {
  return (
    <li
      {...props}
      className={cn('transition-colors hover:bg-muted/50', FOLLOWS, className)}
      {...followHandlers({ onClick, onAuxClick })}
    />
  )
}
