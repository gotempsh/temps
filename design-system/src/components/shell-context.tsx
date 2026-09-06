// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, useEffect } from 'react'

/* ────────────────────────────────────────────────────────────────────────
   The seam between the one shell (`src/components/Layout.tsx`) and the page
   inside it.

   Two things cross it, and only two:

     query   the filter box in the top bar. The shell uses it to narrow the
             left rail; the guide uses the same string to search its headings
             and taste rules, so one box does both and `/` has one meaning.
     toc     what the right rail lists. The guide feeds it the headings of
             the section it is showing, a reference page feeds it its block
             list — the rail itself is written once, here-adjacent, not twice.

   Kept in its own module so `Layout` can import the guide's section list and
   the guide can import the seam without the two files importing each other.
   ──────────────────────────────────────────────────────────────────────── */

/**
 * A page that is a full-bleed mockup (the console, the landing page, the
 * status page, the agent conversation) cancels the content column's padding
 * with this, so it reaches the column's edges instead of floating inside it.
 */
export const PAGE_BLEED = '-mx-4 -my-6 sm:-mx-6'

export type TocEntry = { id: string; text: string }

export type Shell = {
  query: string
  setQuery: (q: string) => void
  setToc: (entries: readonly TocEntry[]) => void
}

/** Outside the shell (`/console`, `/landing`, `/status`) the seam is inert. */
const INERT: Shell = { query: '', setQuery: () => {}, setToc: () => {} }

export const ShellContext = createContext<Shell>(INERT)

export function useShell(): Shell {
  return useContext(ShellContext)
}

/**
 * Publish this page's "on this page" list. Pass a stable array (a module
 * constant, or one memoised on what it derives from): it is the effect's
 * dependency, and a fresh array every render would loop.
 */
export function useDocToc(entries: readonly TocEntry[]): void {
  const { setToc } = useShell()
  useEffect(() => {
    setToc(entries)
    return () => setToc([])
  }, [entries, setToc])
}
