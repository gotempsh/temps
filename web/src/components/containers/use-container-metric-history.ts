// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { containerMetricsGetHistoryOptions } from '@/api/client/@tanstack/react-query.gen'
import { hasMetricHistory } from './container-metric-history'

export interface ContainerMetricHistoryQuery {
  projectId: number
  environmentId: number
  containerId: string
  metric: string
  enabled?: boolean
}

/**
 * A container metric's last hour, as the sparkline reads it. Callers that
 * need to know whether the sparkline is drawn (a row that would otherwise
 * repeat its value) share this query instead of sending another request.
 */
export function useContainerMetricHistory({
  projectId,
  environmentId,
  containerId,
  metric,
  enabled = true,
}: ContainerMetricHistoryQuery) {
  return useQuery({
    ...containerMetricsGetHistoryOptions({
      path: {
        project_id: projectId,
        environment_id: environmentId,
        container_id: containerId,
      },
      query: { metric, range: '1h' },
    }),
    staleTime: 30_000,
    refetchInterval: 30_000,
    enabled,
    // Metrics store disabled -> the endpoint returns 503; avoid retry spam.
    retry: false,
  })
}

/** Whether the metric has history to chart; see `hideWithoutHistory`. */
export function useHasContainerMetricHistory(
  query: ContainerMetricHistoryQuery
) {
  const { data } = useContainerMetricHistory(query)
  return query.enabled !== false && hasMetricHistory(data)
}
