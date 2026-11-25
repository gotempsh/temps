// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { useEffect, useMemo } from 'react'
import {
  hasAnalyticsEventsOptions,
  hasTracesOptions,
  listCustomDomainsForProjectOptions,
  listMonitorsOptions,
  listProjectServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  UNKNOWN,
  countStatus,
  rememberStates,
  rememberedStates,
  withRemembered,
  type AreaStatus,
  type AreaStatuses,
} from './area-state'

// One small request per area, shared with the area's own page where the
// request is the same, and not repeated for a minute.
const QUERY = { staleTime: 60_000, retry: false } as const

/** The data of a query that succeeded; a stable reference between renders. */
function settled<T>(query: {
  data: T | undefined
  isSuccess: boolean
}): T | undefined {
  return query.isSuccess ? query.data : undefined
}

/** Loading, failed and refused all mean "not known", never "not set up". */
function answered<T>(
  data: T | undefined,
  toStatus: (data: T) => AreaStatus
): AreaStatus {
  return data === undefined ? UNKNOWN : toStatus(data)
}

const received = (yes: boolean): AreaStatus => ({
  state: yes ? 'active' : 'inactive',
})

/**
 * Which views of one service hold anything, by section id: one small
 * request each, shared with the view's own page where it is the same
 * request, and not repeated for a minute. A view with no entry here needs
 * no set-up and is always a tab.
 */
export function useServiceAreaStates(
  projectId: number | undefined
): AreaStatuses {
  const known = projectId !== undefined
  const path = { project_id: projectId ?? 0 }
  const scope = `service:${projectId ?? ''}`

  const traces = useQuery({
    ...hasTracesOptions({ path }),
    ...QUERY,
    enabled: known,
  })
  const analytics = useQuery({
    ...hasAnalyticsEventsOptions({ path }),
    ...QUERY,
    enabled: known,
  })
  const monitors = useQuery({
    ...listMonitorsOptions({ path }),
    ...QUERY,
    enabled: known,
  })
  const domains = useQuery({
    ...listCustomDomainsForProjectOptions({ path }),
    ...QUERY,
    enabled: known,
  })
  const databases = useQuery({
    ...listProjectServicesOptions({ path }),
    ...QUERY,
    enabled: known,
  })

  const hasTraces = settled(traces)
  const hasEvents = settled(analytics)
  const monitorList = settled(monitors)
  const domainList = settled(domains)
  const databaseList = settled(databases)

  const live = useMemo<AreaStatuses>(
    () => ({
      traces: answered(hasTraces, (data) => received(data.has_traces)),
      analytics: answered(hasEvents, (data) => received(data.has_events)),
      uptime: answered(monitorList, (list) =>
        countStatus(list.length, 'monitor')
      ),
      domains: answered(domainList, (data) =>
        countStatus(data.domains.length, 'custom domain')
      ),
      databases: answered(databaseList, (list) =>
        countStatus(list.length, 'database')
      ),
    }),
    [hasTraces, hasEvents, monitorList, domainList, databaseList]
  )

  useEffect(() => {
    if (known) rememberStates(scope, live)
  }, [known, scope, live])

  return useMemo(
    () => (known ? withRemembered(live, rememberedStates(scope)) : live),
    [known, scope, live]
  )
}
