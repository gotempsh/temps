// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectsHealthOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

export function useDashboardHealth(
  projectIds: number[],
  startDate?: string,
  endDate?: string
) {
  return useQuery({
    ...getProjectsHealthOptions({
      query: {
        project_ids: projectIds.join(','),
        start_time: startDate,
        end_time: endDate,
      },
    }),
    enabled: projectIds.length > 0,
    staleTime: 1000 * 30,
    refetchInterval: 1000 * 30,
  })
}
