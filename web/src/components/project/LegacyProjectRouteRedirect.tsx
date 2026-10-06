// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Navigate, useLocation } from 'react-router'
import {
  legacyProjectRouteTarget,
  renameProjectRoutePrefix,
  type LegacyProjectRoute,
} from '@/lib/project-settings-sections'

/**
 * Sends a standalone route that predates the combined settings pages to the
 * section that replaced it, so bookmarks and old links land on the same page
 * the sub-navigation highlights.
 */
export function LegacyProjectRouteRedirect({
  projectSlug,
  route,
}: {
  projectSlug: string
  route: LegacyProjectRoute
}) {
  const { search, hash } = useLocation()
  return (
    <Navigate
      to={`/projects/${projectSlug}/${legacyProjectRouteTarget(route, search)}${hash}`}
      replace
    />
  )
}

/**
 * Redirects every URL under a renamed route segment, detail pages included
 * (`logs/:id` → `request-logs/:id`), keeping the query string.
 */
export function RenamedProjectRouteRedirect({
  projectSlug,
  from,
  to,
}: {
  projectSlug: string
  from: string
  to: string
}) {
  const { pathname, search, hash } = useLocation()
  const target =
    renameProjectRoutePrefix(pathname, from, to) ??
    `/projects/${projectSlug}/${to}`
  return <Navigate to={`${target}${search}${hash}`} replace />
}
