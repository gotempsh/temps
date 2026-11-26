// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLocation } from 'react-router'
import { useIsMobile } from '@/components/hooks/use-mobile'
import {
  type PageWidth,
  projectEntityViewOf,
  projectLocationFor,
  projectPageWidth,
  projectRoot,
  projectTabNamesPage,
} from '@/lib/project-navigation'

interface ProjectRef {
  slug: string
  source_type?: string | null
}

/** The current URL below the project's root, e.g. `errors/42`. */
function useProjectRoute(slug: string): string {
  const { pathname } = useLocation()
  const root = projectRoot(slug)
  return pathname.startsWith(root) ? pathname.slice(root.length) : ''
}

/** The section and tab of `project`'s menu the current URL belongs to. */
export function useProjectLocation(project: ProjectRef) {
  return projectLocationFor(project, useProjectRoute(project.slug))
}

/**
 * Whether something above the page already shows its name: the tab of a
 * view with several pages, or, on a phone, the button that names the view.
 * A view with one page has no tabs, so there its page prints its own title.
 */
export function useProjectTabNamesPage(project: ProjectRef): boolean {
  const route = useProjectRoute(project.slug)
  const isMobile = useIsMobile()
  const location = projectLocationFor(project, route)
  if (!projectTabNamesPage(route, location)) return false
  const pages = projectEntityViewOf(project, location)?.tabs.length ?? 0
  return isMobile || pages > 1
}

/** How wide the body of the current page may grow (form or full). */
export function useProjectPageWidth(project: ProjectRef): PageWidth {
  const location = useProjectLocation(project)
  const { search } = useLocation()
  return projectPageWidth(location, search)
}
