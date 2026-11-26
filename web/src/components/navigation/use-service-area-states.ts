// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { useEffect, useMemo } from 'react'
import { hasAnalyticsEventsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  UNKNOWN,
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
 * Which views of one service hold anything, by section id. Only Analytics
 * depends on it: a website receives page views and an API never does, so
 * it is a row once it has some. Every other view is always a row, and what
 * it holds nothing of yet (traces, monitors) is a tab showing how to set it
 * up. One small request, shared with the Analytics page, and not repeated
 * for a minute.
 */
export function useServiceAreaStates(
  projectId: number | undefined
): AreaStatuses {
  const known = projectId !== undefined
  const path = { project_id: projectId ?? 0 }
  const scope = `service:${projectId ?? ''}`

  const analytics = useQuery({
    ...hasAnalyticsEventsOptions({ path }),
    ...QUERY,
    enabled: known,
  })
  const hasEvents = settled(analytics)

  const live = useMemo<AreaStatuses>(
    () => ({
      analytics: answered(hasEvents, (data) => received(data.has_events)),
    }),
    [hasEvents]
  )

  useEffect(() => {
    if (known) rememberStates(scope, live)
  }, [known, scope, live])

  return useMemo(
    () => (known ? withRemembered(live, rememberedStates(scope)) : live),
    [known, scope, live]
  )
}
