// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where an old `/projects/:slug/analytics/requests/*` link now lives. The
 * request-log list and detail routes are identical under `request-logs`, so
 * the remaining path and query string carry over unchanged.
 */
export function requestLogsRedirectPath(
  projectSlug: string,
  rest: string,
  search: string
): string {
  const suffix = rest.replace(/^\/+/, '')
  return `/projects/${projectSlug}/request-logs${suffix ? `/${suffix}` : ''}${search}`
}
