// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef } from 'react'
import { useQuery } from '@tanstack/react-query'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  getProjectDeploymentsOptions,
  listContainersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { portlessWorkerHealth } from '@/components/containers/container-workload'
import { useProjectsMonitorHealth } from '@/hooks/useProjectsMonitorHealth'
import {
  environmentLiveStatus,
  type EnvironmentLiveStatus,
} from '@/lib/project-environment-selection'
import {
  containerHealthIssues,
  healthEnvironment,
} from '@/lib/project-failure-state'
import { isActiveDeploymentStatus } from '@/lib/recent-deployments'

/** How often a running deployment is re-read, and an idle one. */
const DEPLOYING_REFETCH_MS = 5_000
const IDLE_REFETCH_MS = 30_000
/** How often live container state is re-read for the Degraded check. */
const CONTAINERS_REFETCH_MS = 30_000

export interface EnvironmentHealth {
  status: EnvironmentLiveStatus | undefined
  /** The environment's newest deployment, which a Failed status opens. */
  latestDeploymentId: number | undefined
  /** A background worker: nothing to visit, so no reachability either. */
  isWorker: boolean
}

/**
 * The selected environment's state for the project bar: its newest
 * deployment, its live containers, and, for the environment the uptime
 * monitors watch, their verdict. Each query is bounded to one environment
 * and polls slowly unless a deployment is running.
 */
export function useEnvironmentHealth({
  project,
  environment,
  environments,
  enabled,
  onDeploymentSettled,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse | undefined
  environments: EnvironmentResponse[]
  /** Off for apps hosted elsewhere and while "All environments" is picked. */
  enabled: boolean
  /** A running deployment finished: the environment records are stale. */
  onDeploymentSettled: () => void
}): EnvironmentHealth {
  const active = enabled && !!environment
  const environmentId = environment?.id ?? 0
  const live = !!environment?.current_deployment_id
  const sleeping = !!environment?.sleeping

  const deployments = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: { environment_id: environmentId, page: 1, per_page: 1 },
    }),
    enabled: active,
    retry: false,
    refetchInterval: (query) => {
      const status = query.state.data?.deployments?.[0]?.status
      return status && isActiveDeploymentStatus(status)
        ? DEPLOYING_REFETCH_MS
        : IDLE_REFETCH_MS
    },
  })
  const latestDeployment = deployments.data?.deployments?.[0]

  const containers = useQuery({
    ...listContainersOptions({
      path: { project_id: project.id, environment_id: environmentId },
    }),
    enabled: active && live && !sleeping,
    refetchInterval: CONTAINERS_REFETCH_MS,
    staleTime: CONTAINERS_REFETCH_MS / 2,
    retry: false,
  })

  // Monitors check the project's production environment, so their verdict
  // only describes that one.
  const watched = healthEnvironment(environments)
  const monitorsWatchThis = active && watched?.id === environment?.id
  const monitors = useProjectsMonitorHealth(
    monitorsWatchThis ? [project.id] : []
  )
  const monitorStatus = monitorsWatchThis
    ? monitors.data?.projects?.[String(project.id)]?.status
    : undefined

  const unhealthyContainers = useMemo(
    () =>
      containerHealthIssues(containers.data?.containers, { sleeping }).length,
    [containers.data, sleeping]
  )

  // When a deployment finishes, the environment's pointer has moved: re-read
  // it so "Deploying" turns into "Live" rather than "Not deployed".
  const latestStatus = latestDeployment?.status
  const previousStatus = useRef(latestStatus)
  useEffect(() => {
    const previous = previousStatus.current
    previousStatus.current = latestStatus
    if (
      previous !== undefined &&
      isActiveDeploymentStatus(previous) &&
      !(latestStatus && isActiveDeploymentStatus(latestStatus))
    )
      onDeploymentSettled()
  }, [latestStatus, onDeploymentSettled])

  const supportsWorker =
    project.project_type === 'server' && project.preset !== 'docker-compose'
  const isWorker =
    supportsWorker && portlessWorkerHealth(containers.data?.containers) !== null

  return {
    status:
      active && environment
        ? environmentLiveStatus(environment, {
            latestDeployment,
            unhealthyContainers,
            monitorStatus,
          })
        : undefined,
    latestDeploymentId: latestDeployment?.id,
    isWorker,
  }
}
