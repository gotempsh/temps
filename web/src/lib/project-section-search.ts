// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectSectionLink } from './project-navigation'

/**
 * Whether a section page matches the contextual page finder query. Keywords
 * let a page be found by what it shows ("globe", "flow") rather than only by
 * its title or URL.
 */
export function projectSectionLinkMatches(
  link: Pick<ProjectSectionLink, 'title' | 'url' | 'keywords'>,
  search: string
): boolean {
  const query = search.trim().toLowerCase()
  if (!query) return true
  return [link.title, link.url, ...(link.keywords ?? [])].some((term) =>
    term.toLowerCase().includes(query)
  )
}
