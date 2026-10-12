// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { generatePath, parsePath, type Params, type Path } from 'react-router'

/**
 * Where a legacy URL should land.
 *
 * Keeps the incoming query string and hash so a bookmarked
 * `/old-page?search=x&tab=y#row-4` still arrives with its filters. Pieces the
 * destination spells out itself win: `to="/projects/new?source=drop"` keeps
 * `source=drop` even if the old link carried a different `source`.
 *
 * `:param` and `*` segments in `to` are filled from the matched route, so a
 * legacy `/old/:id` can redirect to `/new/:id`.
 */
export function buildRedirectTarget({
  to,
  search,
  hash,
  params,
}: {
  to: string
  search: string
  hash: string
  params: Params
}): Partial<Path> {
  const target = parsePath(to)
  const pathname = target.pathname
    ? fillParams(target.pathname, params)
    : undefined

  const merged = new URLSearchParams(search)
  for (const [key, value] of new URLSearchParams(target.search ?? '')) {
    merged.set(key, value)
  }
  const mergedSearch = merged.toString()

  return {
    pathname,
    search: mergedSearch ? `?${mergedSearch}` : '',
    hash: target.hash || hash,
  }
}

function fillParams(pathname: string, params: Params): string {
  const hasPlaceholder = /(^|\/)(:[^/]+|\*)(\/|$)/.test(pathname)
  return hasPlaceholder ? generatePath(pathname, params) : pathname
}
