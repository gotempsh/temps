// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { cn } from './lib/cn'

/**
 * A project's identity mark: its favicon or uploaded logo, or a monogram.
 * It sits wherever the name sits (ledger row, page title, palette row,
 * breadcrumb) and only there; it never becomes a card or a hero.
 *
 * Sizes are fixed so the mark is never the loudest thing in a row: 16px in
 * rows and lists, 24px next to a page title. A favicon may carry its own
 * colours, the one exception to "colour is status" alongside provider logos,
 * because a logo the reader recognises is worth more than palette purity;
 * at 16px it cannot compete with a state glyph anyway.
 *
 * Fallback is a monogram (first letter, ink on paper, 1px border, mono), used
 * while nothing is known, when the fetch fails, and for projects with no
 * domain yet. It is deterministic and never coloured, so an unknown project
 * looks unknown rather than randomly branded.
 *
 * Source (server side, not this component's business): the console fetches
 * `/favicon.ico` or `<link rel="icon">` from the project's production domain
 * after a successful deploy, stores it, and serves it from its own origin
 * (`/api/projects/{id}/icon`). Nothing is hot-linked from the browser, so a
 * self-hosted console leaks no visitor information to the project's host.
 */
export function ProjectMark({ name, icon, size = 16, className }: { name: string; icon?: string | null; size?: 16 | 24; className?: string }) {
  const [broken, setBroken] = useState(false)
  const letter = (name.trim()[0] ?? '?').toUpperCase()
  const box = size === 24 ? 'h-6 w-6 text-xs' : 'h-4 w-4 text-[9px]'
  if (icon && !broken) {
    return <img src={icon} alt="" width={size} height={size} onError={() => setBroken(true)} className={cn('shrink-0 border bg-background object-contain', box, className)} />
  }
  return (
    <span aria-hidden className={cn('inline-flex shrink-0 items-center justify-center border bg-background font-mono font-medium leading-none text-foreground', box, className)}>
      {letter}
    </span>
  )
}
