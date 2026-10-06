// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { safeReturnTo } from '@/lib/safe-return-to'

/**
 * Detail pages (a variable, a secret) can be opened from more than one list.
 * The list passes where it was in router state; the detail page's back link
 * returns there, and falls back to the canonical list for a deep link, a
 * reload in a new tab, or anything that does not belong to this project.
 *
 * This is the router-state counterpart of `?returnTo=` (see
 * `safe-return-to.ts`): same in-app path validation, plus a same-project
 * restriction, and nothing in the URL to share or bookmark.
 */

export type DetailReturnState = { returnTo: string }

/** Router state a list attaches to its detail links. */
export function detailReturnState(location: {
  pathname: string
  search: string
}): DetailReturnState {
  return { returnTo: `${location.pathname}${location.search}` }
}

/**
 * Where a detail page's back link goes. Only same-project console paths are
 * honoured — router state is client-controlled and must not become an open
 * redirect or a jump into another project.
 */
export function detailReturnPath(
  state: unknown,
  projectSlug: string,
  fallback: string
): string {
  const raw = (state as Partial<DetailReturnState> | null)?.returnTo
  const returnTo = typeof raw === 'string' ? safeReturnTo(raw) : null
  if (!returnTo) return fallback
  const projectRoot = `/projects/${projectSlug}`
  return returnTo === projectRoot ||
    returnTo.startsWith(`${projectRoot}/`) ||
    returnTo.startsWith(`${projectRoot}?`)
    ? returnTo
    : fallback
}
