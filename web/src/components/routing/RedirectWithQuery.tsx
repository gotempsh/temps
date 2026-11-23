// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, useLocation, useParams } from 'react-router'
import { fillRouteParams, redirectTarget } from '@/lib/redirect-target'

/**
 * `<Navigate replace>` that keeps the current query string and hash. `:name`
 * segments of `to` take the matched route's parameters.
 */
export function RedirectWithQuery({ to }: { to: string }) {
  const { search, hash } = useLocation()
  const params = useParams()
  return (
    <Navigate
      to={redirectTarget(fillRouteParams(to, params), search, hash)}
      replace
    />
  )
}
