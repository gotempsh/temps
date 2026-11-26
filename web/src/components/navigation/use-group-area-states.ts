// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQueries } from '@tanstack/react-query'
import { useEffect, useMemo } from 'react'
import {
  hasAnalyticsEventsOptions,
  hasTracesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  fanoutMembers,
  groupDatabaseQueries,
  groupDomainQueries,
  type GroupMember,
} from '@/lib/group-scoped-lists'
import {
  servicesWithSection,
  type ServiceKindSource,
} from '@/lib/service-kinds'
import {
  UNKNOWN,
  rememberStates,
  rememberedStates,
  withRemembered,
  type AreaStatus,
  type AreaStatuses,
} from './area-state'

const QUERY = { staleTime: 60_000, retry: false } as const

/** A project's service: what a request needs and what tells its kind. */
type GroupService = GroupMember & ServiceKindSource

interface Answer {
  /** How many of the services use the area; undefined while not known. */
  using: number | undefined
  asked: number
}

/**
 * One area's state from one answer per service: in use as soon as a service
 * uses it, not in use only once every service has answered that it does not.
 */
function summarize(
  results: readonly { isSuccess: boolean; data: unknown }[],
  uses: (data: never) => boolean
): Answer {
  const answered = results.filter((result) => result.isSuccess)
  const using = answered.filter((result) => uses(result.data as never)).length
  return {
    using: using > 0 || answered.length === results.length ? using : undefined,
    asked: results.length,
  }
}

function toStatus({ using, asked }: Answer): AreaStatus {
  if (using === undefined || asked === 0) return UNKNOWN
  if (using === 0) return { state: 'inactive' }
  return {
    state: 'active',
    metric: asked === 1 ? undefined : `${using} of ${asked} services`,
  }
}

/**
 * Which views of a project hold anything for any of its services, by
 * section id: one small request per service, shared with the view's own
 * page where it is the same request, and not repeated for a minute. A
 * project with more services than one list requests is asked about the
 * first ones only, so "not in use" is then never concluded.
 */
export function useGroupAreaStates(group: {
  id: number
  projects: readonly GroupService[]
}): AreaStatuses {
  const scope = `project:${group.id}`
  const { requested, truncated } = fanoutMembers(group.projects)
  const deployed = servicesWithSection(requested, 'databases')
  const hosted = servicesWithSection(requested, 'domains')

  const traces = useQueries({
    queries: requested.map((member) => ({
      ...hasTracesOptions({ path: { project_id: member.id } }),
      ...QUERY,
    })),
  })
  const analytics = useQueries({
    queries: requested.map((member) => ({
      ...hasAnalyticsEventsOptions({ path: { project_id: member.id } }),
      ...QUERY,
    })),
  })
  const domains = useQueries({
    queries: groupDomainQueries(hosted).map((query) => ({
      ...query,
      ...QUERY,
    })),
  })
  const databases = useQueries({
    queries: groupDatabaseQueries(deployed).map((query) => ({
      ...query,
      ...QUERY,
    })),
  })

  // A stable key per area, so the memo below does not recompute each render.
  const key = (
    results: readonly { isSuccess: boolean; dataUpdatedAt: number }[]
  ) => results.map((r) => `${r.isSuccess ? 1 : 0}:${r.dataUpdatedAt}`).join(',')
  const tracesKey = key(traces)
  const analyticsKey = key(analytics)
  const domainsKey = key(domains)
  const databasesKey = key(databases)

  const live = useMemo<AreaStatuses>(() => {
    const status = (answer: Answer): AreaStatus => {
      const result = toStatus(answer)
      // Some services were not asked: "none of them" cannot be concluded.
      return truncated && result.state === 'inactive' ? UNKNOWN : result
    }
    return {
      traces: status(
        summarize(traces, (data: { has_traces: boolean }) => data.has_traces)
      ),
      analytics: status(
        summarize(analytics, (data: { has_events: boolean }) => data.has_events)
      ),
      domains: status(
        summarize(
          domains,
          (data: { domains: unknown[] }) => data.domains.length > 0
        )
      ),
      databases: status(
        summarize(databases, (data: unknown[]) => data.length > 0)
      ),
    }
    // The keys stand for the results, which are new arrays on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tracesKey, analyticsKey, domainsKey, databasesKey, truncated])

  useEffect(() => {
    rememberStates(scope, live)
  }, [scope, live])

  return useMemo(
    () => withRemembered(live, rememberedStates(scope)),
    [scope, live]
  )
}
