// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getDashboardProjectsAnalyticsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

export function useDashboardAnalytics(
  projectIds: number[],
  startDate: string,
  endDate: string
) {
  return useQuery({
    ...getDashboardProjectsAnalyticsOptions({
      query: {
        project_ids: projectIds.join(','),
        start_date: startDate,
        end_date: endDate,
      },
    }),
    enabled: projectIds.length > 0,
    staleTime: 1000 * 60 * 5, // 5 minutes
    refetchInterval: 1000 * 60, // Refetch every minute
  })
}
