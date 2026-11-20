// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { To } from 'react-router'

/**
 * Builds the destination of a legacy redirect.
 *
 * The incoming query string and hash ride along so a bookmarked or shared link
 * such as `/monitoring?search=x&tab=y` keeps its filters. When the target
 * already has parameters of its own (`/projects/new?source=drop`) those win on
 * a clash, because they are what makes the redirect mean what it says.
 */
export function redirectTarget(to: string, search: string, hash: string): To {
  const hashStart = to.indexOf('#')
  const withoutHash = hashStart === -1 ? to : to.slice(0, hashStart)
  const targetHash = hashStart === -1 ? '' : to.slice(hashStart)
  const queryStart = withoutHash.indexOf('?')
  const pathname =
    queryStart === -1 ? withoutHash : withoutHash.slice(0, queryStart)
  const query = new URLSearchParams(
    queryStart === -1 ? '' : withoutHash.slice(queryStart + 1)
  )
  // Keys the target itself sets; every incoming value of any other key rides
  // along, so a repeated parameter (`?attr=a&attr=b`) keeps all its values.
  const targetKeys = new Set(query.keys())
  new URLSearchParams(search).forEach((value, key) => {
    if (!targetKeys.has(key)) query.append(key, value)
  })
  const merged = query.toString()
  return {
    pathname,
    search: merged ? `?${merged}` : '',
    hash: targetHash || hash,
  }
}
