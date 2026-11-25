// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Rocket } from 'lucide-react'
import { useLocation, useSearchParams } from 'react-router'
import { listsAsTab } from '@/components/navigation/area-state'
import {
  EntityViewTabs,
  type EntityView,
} from '@/components/navigation/EntityViewTabs'
import { PageTabStrip } from '@/components/navigation/PageTabStrip'
import { SERVICE_VIEW_MISSING } from '@/components/navigation/service-areas'
import { useServiceAreaStates } from '@/components/navigation/use-service-area-states'
import { ENVIRONMENT_PARAM } from '@/lib/project-environment-selection'
import {
  projectEntityViewsFor,
  projectRoot,
  type ProjectNavLocation,
} from '@/lib/project-navigation'
import { useProjectTourActive } from '@/lib/project-tour'

/**
 * A service's views as the tabs of its page (Deployments, Logs, Errors, the
 * data it has, then Settings), the pages of the view shown as a second row,
 * and the pages behind one of those as a third. They navigate between URLs, so
 * they are links (new tab, copy link and history work), and each keeps the
 * selected environment.
 *
 * On a narrow screen a strip scrolls sideways; the current tab is scrolled
 * into view so a page reached by link (Check-ins, the last Uptime tab) does
 * not open with its own tab hidden.
 */
export function ProjectSectionTabs({
  project,
  location,
}: {
  project: { id?: number; slug: string; source_type?: string | null }
  location: ProjectNavLocation
}) {
  const [searchParams] = useSearchParams()
  const { pathname } = useLocation()
  // The quick tour points at the views, so each is a tab while it runs.
  const tourActive = useProjectTourActive()
  const statuses = useServiceAreaStates(project.id)
  const { section, tab, view } = location
  const env = searchParams.get(ENVIRONMENT_PARAM)
  const query = env ? `?${ENVIRONMENT_PARAM}=${encodeURIComponent(env)}` : ''
  const root = projectRoot(project.slug)
  const to = (url: string) => `${root}/${url}${query}`
  // "Add hosting" belongs to no view; none is marked while it is shown.
  const onHosting =
    pathname === `${root}/hosting` || pathname.startsWith(`${root}/hosting/`)
  const entityViews = projectEntityViewsFor(project)
  const shown = onHosting
    ? undefined
    : entityViews.find((item) => item.sections.some((s) => s.id === section.id))
  const views: EntityView[] = entityViews.map((item) => ({
    key: item.id,
    title: item.title,
    icon: item.icon,
    to: to(item.tabs[0].url),
    active: item === shown,
    inUse: listsAsTab(statuses[item.id]),
    missing: SERVICE_VIEW_MISSING[item.id],
    tour: item.id,
  }))
  if (project.source_type === 'external')
    views.push({
      key: 'hosting',
      title: 'Add hosting',
      icon: Rocket,
      to: `${root}/hosting`,
      active: onHosting,
      inUse: true,
    })
  const tabs = shown?.tabs ?? []
  const pages = shown ? tab.views : undefined
  return (
    <>
      <EntityViewTabs
        label={`${project.slug} views`}
        views={views}
        showAll={tourActive}
        // The rows under it sit right below.
        className={tabs.length > 1 || pages ? 'mb-3' : undefined}
      />
      <PageTabStrip
        level="secondary"
        label={`${shown?.title ?? section.title} pages`}
        className={pages ? 'mb-2' : undefined}
        icons={false}
        tabs={tabs.map((item) => ({
          key: item.url,
          title: item.title,
          icon: item.icon,
          to: to(item.url),
          active: item.url === tab.url,
        }))}
      />
      {pages && (
        <PageTabStrip
          level="tertiary"
          label={`${section.title} ${tab.title.toLowerCase()} pages`}
          tabs={pages.map((item) => ({
            key: item.url,
            title: item.title,
            icon: item.icon,
            to: to(item.url),
            active: item.url === view?.url,
          }))}
        />
      )}
    </>
  )
}
