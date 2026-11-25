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
import { PageTabStrip } from './PageTabStrip'
import { useGroupAreaStates } from './use-group-area-states'

/**
 * A project's views as the tabs of its page (Deployments, Logs, Errors and
 * the data its services have), with the pages of the view shown as a second
 * row (Databases and its Backups). Only the views its services can have are
 * there: a project of static sites has no Databases. The strip owns the gutters here, so the
 * page inside lines up with it and adds none of its own.
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
        // The pages of the view shown sit right under it.
        className={tab ? 'mb-3' : undefined}
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
        }))}
      />
      {section.tabs && tab && (
        <PageTabStrip
          level="secondary"
          icons={false}
          label={`${section.title} pages`}
          tabs={section.tabs.map((item) => ({
            key: item.url,
            title: item.title,
            icon: item.icon,
            to: groupPathHref(group.slug, item.url),
            active: item.url === tab.url,
          }))}
        />
      )}
      {/* The selected tab names the page; the Overview names the project. */}
      <PageNamedByChromeContext.Provider value={section.id !== 'overview'}>
        {children}
      </PageNamedByChromeContext.Provider>
    </PageContainer>
  )
}
