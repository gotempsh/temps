// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectDeployments, type DeploymentResponse } from '@/api/client'
import { findLastSuccessfulDeployment } from '@/lib/project-failure-state'
import { useQuery } from '@tanstack/react-query'

/**
 * The newest deployment in `failed`'s environment that is older than it and
 * can be rolled back to, searching as far back in history as needed (see
 * `findLastSuccessfulDeployment`). `undefined` while searching, `null` when
 * there is none.
 */
export function useLastSuccessfulDeployment(
  failed:
    | Pick<
        DeploymentResponse,
        'id' | 'project_id' | 'environment_id' | 'created_at'
      >
    | undefined
) {
  return useQuery({
    queryKey: [
      'lastSuccessfulDeployment',
      failed?.project_id,
      failed?.environment_id,
      failed?.id,
    ],
    enabled: !!failed,
    queryFn: async ({ signal }) => {
      if (!failed) return null
      return findLastSuccessfulDeployment(async (page, perPage) => {
        const { data } = await getProjectDeployments({
          path: { id: failed.project_id },
          query: {
            environment_id: failed.environment_id,
            page,
            per_page: perPage,
          },
          signal,
          throwOnError: true,
        })
        return data.deployments
      }, failed)
    },
  })
}
