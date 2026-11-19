// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  getUniqueCountsOptions,
  hasAnalyticsEventsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  analyticsCheckPassed,
  analyticsCheckWindow,
  buildProjectAnalyticsCountRequest,
} from '@/lib/project-analytics-summary'

/**
 * The analytics setup wizard's "is it installed?" check: call `start` when the
 * user says it is, and `passed` turns true once an event proves it.
 */
export function useAnalyticsCheck(projectId: number, waiting: boolean) {
  const { data: hasEventsData } = useQuery({
    ...hasAnalyticsEventsOptions({ path: { project_id: projectId } }),
    enabled: !!projectId,
    refetchInterval: waiting ? 2000 : false,
    refetchOnWindowFocus: false,
  })
  const [check, setCheck] = useState<{
    startedAt: number
    hadEventsBefore: boolean
  } | null>(null)
  const start = () =>
    setCheck({
      startedAt: Date.now(),
      // Not known yet counts as "had some": that asks for a new event, which
      // can only under-report success, never claim it.
      hadEventsBefore: hasEventsData?.has_events !== false,
    })
  const eventsSinceCheck = useQuery({
    ...getUniqueCountsOptions(
      buildProjectAnalyticsCountRequest(
        projectId,
        analyticsCheckWindow(check?.startedAt ?? 0),
        'page_views'
      )
    ),
    enabled: waiting && check?.hadEventsBefore === true,
    refetchInterval: 2000,
    refetchOnWindowFocus: false,
  })

  return {
    start,
    hadEventsBefore: check?.hadEventsBefore ?? false,
    passed: analyticsCheckPassed({
      hadEventsBefore: check?.hadEventsBefore ?? true,
      hasEvents: hasEventsData?.has_events,
      eventsSinceCheck: eventsSinceCheck.data?.count,
    }),
  }
}
