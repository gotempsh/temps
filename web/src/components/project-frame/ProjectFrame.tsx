// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageNamedByChromeContext, PageTitleLevelContext } from '@temps-sdk/ds'
import type { ReactNode } from 'react'
import type { ProjectResponse } from '@/api/client'
import {
  useProjectLocation,
  useProjectTabNamesPage,
} from './use-project-location'
import { ProjectEnvironmentProvider } from './ProjectEnvironmentProvider'
import { ProjectSectionTabs } from './ProjectSectionTabs'

/**
 * Shares the environment chosen in the bar with every project page. The bar
 * names the project as the page's `h1`, so page titles inside are `h2`.
 */
export function ProjectFrame({
  project,
  children,
}: {
  project: ProjectResponse
  children: ReactNode
}) {
  const location = useProjectLocation(project)
  return (
    // Keyed: the remembered and default environments are read from storage
    // on mount, so going from one project to another must remount it.
    <ProjectEnvironmentProvider
      key={project.id}
      project={project}
      scope={location.section.environmentScope}
    >
      <PageTitleLevelContext.Provider value="h2">
        {children}
      </PageTitleLevelContext.Provider>
    </ProjectEnvironmentProvider>
  )
}

/**
 * The current section's tabs above its page. When the selected tab already
 * names the page, the page's own title is kept for screen readers only.
 */
export function ProjectSectionChrome({
  project,
  children,
}: {
  project: ProjectResponse
  children: ReactNode
}) {
  const location = useProjectLocation(project)
  const tabNamesPage = useProjectTabNamesPage(project)
  return (
    <>
      <ProjectSectionTabs projectSlug={project.slug} location={location} />
      <PageNamedByChromeContext.Provider value={tabNamesPage}>
        {children}
      </PageNamedByChromeContext.Provider>
    </>
  )
}
