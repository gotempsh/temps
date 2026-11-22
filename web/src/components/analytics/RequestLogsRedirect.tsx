// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, useLocation, useParams } from 'react-router'
import { requestLogsRedirectPath } from './analytics-legacy-routes'

/**
 * Request logs used to be mounted a second time under analytics; keep old
 * links working by sending them to the canonical Request logs page, with the
 * query string and hash intact.
 */
export function RequestLogsRedirect({ projectSlug }: { projectSlug: string }) {
  const { '*': rest = '' } = useParams()
  const { search, hash } = useLocation()
  return (
    <Navigate
      to={`${requestLogsRedirectPath(projectSlug, rest, search)}${hash}`}
      replace
    />
  )
}
