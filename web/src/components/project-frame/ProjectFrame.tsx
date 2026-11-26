// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageNamedByChromeContext, PageTitleLevelContext } from '@temps-sdk/ds'
import type { ReactNode } from 'react'
import type { ProjectResponse } from '@/api/client'
import {
  useProjectLocation,
  useProjectPageWidth,
  useProjectTabNamesPage,
} from './use-project-location'
import { ProjectEnvironmentProvider } from './ProjectEnvironmentProvider'
import { projectEnvironmentScope } from '@/lib/project-navigation'
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
      scope={projectEnvironmentScope(location)}
    >
      <PageTitleLevelContext.Provider value="h2">
        {children}
      </PageTitleLevelContext.Provider>
    </ProjectEnvironmentProvider>
  )
}

/**
 * The pages of the view shown, as one row of tabs above the page. When a
 * selected tab already names the page, the page's own title is kept for
 * screen readers only.
 *
 * Every page of a service shares one column, so the tabs do not move from
 * page to page; a form keeps its body to a readable measure, from the same
 * left edge.
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
  const width = useProjectPageWidth(project)
  return (
    <>
      <ProjectSectionTabs project={project} location={location} />
      <PageNamedByChromeContext.Provider value={tabNamesPage}>
        <div
          data-page-body={width}
          className={width === 'form' ? 'max-w-5xl' : undefined}
        >
          {children}
        </div>
      </PageNamedByChromeContext.Provider>
    </>
  )
}
