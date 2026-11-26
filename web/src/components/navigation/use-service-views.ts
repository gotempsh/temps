// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Rocket } from 'lucide-react'
import { useLocation, useSearchParams } from 'react-router'
import { ENVIRONMENT_PARAM } from '@/lib/project-environment-selection'
import {
  projectEntityViewOf,
  projectEntityViewsFor,
  projectLocationFor,
  projectRoot,
} from '@/lib/project-navigation'
import { useProjectTourActive } from '@/lib/project-tour'
import { listsAsTab } from './area-state'
import type { EntityView } from './EntityViewTabs'
import { SERVICE_VIEW_MISSING } from './service-areas'
import { useServiceAreaStates } from './use-service-area-states'

export interface ServiceRef {
  id?: number
  slug: string
  source_type?: string | null
}

/**
 * A service's views as the sidebar lists them under it: Deployments, Logs,
 * Errors, the data it has, then Settings. Each link keeps the environment
 * chosen in the bar.
 *
 * `listed` are the rows. A view known to hold nothing (no trace received, no
 * domain added) is in `unused` instead, offered under "Add", so what is not
 * set up stays one click away without a row that opens on an empty page.
 */
export function useServiceViews(service: ServiceRef): {
  listed: EntityView[]
  unused: EntityView[]
} {
  const { pathname } = useLocation()
  const [searchParams] = useSearchParams()
  // The quick tour points at the views, so each is a row while it runs.
  const tourActive = useProjectTourActive()
  const statuses = useServiceAreaStates(service.id)
  const env = searchParams.get(ENVIRONMENT_PARAM)
  const query = env ? `?${ENVIRONMENT_PARAM}=${encodeURIComponent(env)}` : ''
  const root = projectRoot(service.slug)
  const inside = pathname === root || pathname.startsWith(`${root}/`)
  // "Add hosting" belongs to no view; none is marked while it is shown.
  const onHosting =
    pathname === `${root}/hosting` || pathname.startsWith(`${root}/hosting/`)
  const shown =
    inside && !onHosting
      ? projectEntityViewOf(
          service,
          projectLocationFor(service, pathname.slice(root.length))
        )
      : undefined
  const views: EntityView[] = projectEntityViewsFor(service).map((item) => ({
    key: item.id,
    title: item.title,
    icon: item.icon,
    to: `${root}/${item.tabs[0].url}${query}`,
    active: item.id === shown?.id,
    inUse: listsAsTab(statuses[item.id]),
    missing: SERVICE_VIEW_MISSING[item.id],
    tour: item.id,
  }))
  if (service.source_type === 'external')
    views.push({
      key: 'hosting',
      title: 'Add hosting',
      icon: Rocket,
      to: `${root}/hosting`,
      active: onHosting,
      inUse: true,
    })
  const isListed = (view: EntityView) => tourActive || view.inUse || view.active
  return {
    listed: views.filter(isListed),
    unused: views.filter((view) => !isListed(view)),
  }
}
