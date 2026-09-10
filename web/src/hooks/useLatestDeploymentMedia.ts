// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getLatestDeploymentMediaOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'

export function useLatestDeploymentMedia(projectIds: number[]) {
  const projectIdsKey = projectIds.join(',')

  return useQuery({
    ...getLatestDeploymentMediaOptions({
      query: { project_ids: projectIdsKey },
    }),
    enabled: projectIds.length > 0,
    staleTime: 1000 * 30,
    refetchInterval: 1000 * 30,
  })
}
