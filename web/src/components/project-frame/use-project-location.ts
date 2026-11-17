// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useLocation } from 'react-router'
import {
  type PageWidth,
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

/** Whether the section's tab strip already shows the current page's name. */
export function useProjectTabNamesPage(project: ProjectRef): boolean {
  const route = useProjectRoute(project.slug)
  return projectTabNamesPage(route, projectLocationFor(project, route))
}

/** How wide the current page may grow (form or full). */
export function useProjectPageWidth(project: ProjectRef): PageWidth {
  const location = useProjectLocation(project)
  const { search } = useLocation()
  return projectPageWidth(location, search)
}
