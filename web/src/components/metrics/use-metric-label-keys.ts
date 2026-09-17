// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { listMetricLabelKeysOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

/** Shared query for filter autocomplete and breakdown key discovery. */
export function useMetricLabelKeys({
  projectId,
  metricName,
  fromIso,
  toIso,
}: {
  projectId: number
  metricName: string
  fromIso: string
  toIso: string
}) {
  return useQuery({
    ...listMetricLabelKeysOptions({
      query: {
        project_id: projectId,
        metric_name: metricName,
        start_time: fromIso,
        end_time: toIso,
      },
    }),
    enabled: !!projectId && metricName.length > 0,
  })
}
