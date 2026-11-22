// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** A path segment naming one record (a deployment, a monitor…), not a section. */
const RECORD_SEGMENT = /^(\d+|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/i

/**
 * Where switching to a sibling service should land: the same section of the
 * other service. `/projects/web/settings/env` → `/projects/api/settings/env`.
 * The path stops before the first record id, since that record belongs to the
 * service being left: `/projects/web/deployments/42` → `/projects/api/deployments`.
 */
export function siblingServicePath(pathname: string, fromSlug: string, toSlug: string): string {
  const prefix = `/projects/${fromSlug}`
  if (pathname !== prefix && !pathname.startsWith(`${prefix}/`)) {
    return `/projects/${toSlug}`
  }
  const sections: string[] = []
  for (const segment of pathname.slice(prefix.length).split('/').filter(Boolean)) {
    if (RECORD_SEGMENT.test(segment)) break
    sections.push(segment)
  }
  return ['/projects', toSlug, ...sections].join('/')
}
