// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getProjectDeploymentsOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  projectDeploymentStatus,
  type ProjectDeploymentStatus,
} from '@/lib/project-deployment-status'
import {
  containerHealthIssues,
  healthEnvironment,
  lastSuccessfulDeployment,
} from '@/lib/project-failure-state'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'

/** How often live container state is re-read for the Degraded check. */
const CONTAINER_HEALTH_REFETCH_MS = 30_000

/**
 * Everything the project header and overview need to say "this project is
 * failing, and here is what to do": the header status (now including Failed
 * and Degraded), the failed latest deployment with its rollback target, and
 * the live containers that are down or crash-looping.
 *
 * The header and overview both call this; React Query de-duplicates the
 * requests, and the environments query shares the header's existing key.
 */
export function useProjectFailureState(
  projectId: number,
  lastDeployment: DeploymentResponse | undefined
) {
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId } }),
    refetchInterval: 5_000,
  })
  const environment = healthEnvironment(environmentsQuery.data)

  const containersQuery = useQuery({
    ...listContainersOptions({
      path: { project_id: projectId, environment_id: environment?.id ?? 0 },
    }),
    enabled: !!environment && !environment.sleeping,
    refetchInterval: CONTAINER_HEALTH_REFETCH_MS,
    staleTime: CONTAINER_HEALTH_REFETCH_MS / 2,
    retry: false,
  })

  const containerIssues = useMemo(
    () =>
      containerHealthIssues(containersQuery.data?.containers, {
        sleeping: environment?.sleeping,
      }),
    [containersQuery.data, environment?.sleeping]
  )

  const failedDeployment =
    lastDeployment?.status === 'failed' ? lastDeployment : undefined

  const deploymentsQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: projectId },
      query: {
        environment_id: failedDeployment?.environment_id,
        per_page: 50,
      },
    }),
    enabled: !!failedDeployment,
  })
  const rollbackTarget = failedDeployment
    ? lastSuccessfulDeployment(
        deploymentsQuery.data?.deployments,
        failedDeployment
      )
    : undefined

  const status: ProjectDeploymentStatus | undefined = projectDeploymentStatus(
    environmentsQuery.data,
    lastDeployment,
    containerIssues.length
  )

  return {
    status,
    environmentsQuery,
    /** Environment whose containers were checked for Degraded. */
    healthEnvironment: environment,
    containerIssues,
    failedDeployment,
    rollbackTarget,
    rollbackTargetLoading: !!failedDeployment && deploymentsQuery.isPending,
  }
}
