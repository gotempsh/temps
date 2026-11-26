// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageNamedByChromeContext } from '@temps-sdk/ds'
import type { ReactNode } from 'react'
import { PageContainer } from '@/components/layout/PageContainer'
import {
  GROUP_NAV_SECTIONS,
  groupPathHref,
  groupSectionHref,
  resolveGroupLocation,
} from '@/lib/console-scope'
import type { GroupMember } from '@/lib/group-scoped-lists'
import {
  sectionAvailability,
  type ServiceKindSource,
} from '@/lib/service-kinds'
import { listsAsTab } from './area-state'
import { EntityViewTabs } from './EntityViewTabs'
import { useGroupAreaStates } from './use-group-area-states'

/**
 * A project's views as the one row of tabs of its page (Deployments, Logs,
 * Errors and the data its services have). A view with several pages
 * (Databases and its Backups) is a tab that opens a menu of them. Only the
 * views its services can have are there: a project of static sites has no
 * Databases. The strip owns the gutters here, so the page inside lines up
 * with it and adds none of its own.
 */
export function GroupAreaChrome({
  group,
  route,
  children,
}: {
  group: {
    id: number
    name: string
    slug: string
    projects: readonly (GroupMember & ServiceKindSource)[]
  }
  /** The sub-path below `/p/:slug`. */
  route: string
  children: ReactNode
}) {
  const statuses = useGroupAreaStates(group)
  const { section, tab } = resolveGroupLocation(route)
  // A record or a form below a view is one task and stands alone.
  const ownPage = route === section.url || tab !== undefined
  if (!ownPage) return <>{children}</>
  return (
    <PageContainer innerClassName="space-y-0">
      <EntityViewTabs
        label={`${group.name} views`}
        // A view none of the project's services can have (domains of a
        // project that is only monitored) is not offered at all.
        views={GROUP_NAV_SECTIONS.filter(
          (item) =>
            item.id === section.id ||
            sectionAvailability(group.projects, item.id).applicable
        ).map((item) => ({
          key: item.id,
          title: item.title,
          icon: item.icon,
          to: groupSectionHref(group.slug, item),
          active: item.id === section.id,
          inUse: listsAsTab(statuses[item.id]),
          missing: item.missing,
          pages: item.tabs?.map((page) => ({
            key: page.url,
            title: page.title,
            icon: page.icon,
            to: groupPathHref(group.slug, page.url),
            active: item.id === section.id && page.url === tab?.url,
          })),
        }))}
      />
      {/* The selected tab names the page; the Overview names the project. */}
      <PageNamedByChromeContext.Provider value={section.id !== 'overview'}>
        {children}
      </PageNamedByChromeContext.Provider>
    </PageContainer>
  )
}
