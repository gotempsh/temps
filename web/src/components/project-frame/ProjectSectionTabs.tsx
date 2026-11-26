// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Rocket } from 'lucide-react'
import { useLocation, useSearchParams } from 'react-router'
import { PageTabStrip } from '@/components/navigation/PageTabStrip'
import { ViewMenuButton } from '@/components/navigation/ViewMenuButton'
import { ENVIRONMENT_PARAM } from '@/lib/project-environment-selection'
import {
  projectEntityViewOf,
  projectRoot,
  type ProjectNavLocation,
} from '@/lib/project-navigation'

/**
 * The pages of the view shown, as the one row of tabs of a service's page.
 * The views themselves (Deployments, Logs, Errors, Settings) are listed
 * under the service in the sidebar; on a phone a button names the one shown
 * and opens that list. A page that holds several (Errors' settings) is a tab
 * that opens a menu, never a second row.
 *
 * The tabs navigate between URLs, so they are links (new tab, copy link and
 * history work), and each keeps the selected environment.
 */
export function ProjectSectionTabs({
  project,
  location,
}: {
  project: { slug: string; source_type?: string | null }
  location: ProjectNavLocation
}) {
  const [searchParams] = useSearchParams()
  const { pathname } = useLocation()
  const { tab, view } = location
  const env = searchParams.get(ENVIRONMENT_PARAM)
  const query = env ? `?${ENVIRONMENT_PARAM}=${encodeURIComponent(env)}` : ''
  const root = projectRoot(project.slug)
  const to = (url: string) => `${root}/${url}${query}`
  // "Add hosting" belongs to no view.
  const onHosting =
    pathname === `${root}/hosting` || pathname.startsWith(`${root}/hosting/`)
  const shown = onHosting ? undefined : projectEntityViewOf(project, location)
  return (
    <>
      <ViewMenuButton
        title={shown?.title ?? 'Add hosting'}
        icon={shown?.icon ?? Rocket}
      />
      {shown && (
        <PageTabStrip
          label={`${shown.title} pages`}
          tabs={shown.tabs.map((item) => ({
            key: item.url,
            title: item.title,
            icon: item.icon,
            to: to(item.url),
            active: item.url === tab.url,
            pages: item.views?.map((page) => ({
              key: page.url,
              title: page.title,
              icon: page.icon,
              to: to(page.url),
              active: item.url === tab.url && page.url === view?.url,
            })),
          }))}
        />
      )}
    </>
  )
}
