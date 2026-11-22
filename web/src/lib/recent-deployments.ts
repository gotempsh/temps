// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'

import { liveListInterval } from './live-refetch'

const ACTIVE_DEPLOYMENT_STATUSES = new Set([
  'pending',
  'running',
  'building',
  'queued',
])

/** Whether a deployment with this status is still building or rolling out. */
export function isActiveDeploymentStatus(status: string): boolean {
  return ACTIVE_DEPLOYMENT_STATUSES.has(status)
}

export function recentDeploymentsRefetchInterval(
  deployments: DeploymentResponse[] | undefined
): number | false {
  return deployments?.some((deployment) =>
    isActiveDeploymentStatus(deployment.status)
  )
    ? 2500
    : false
}

/**
 * Poll interval for the project's full deployments list: fast while a
 * deployment is building, and still polling when idle so a deployment started
 * in the background (webhook, CLI, another operator) shows up on its own.
 */
export function projectDeploymentsRefetchInterval(
  deployments: Pick<DeploymentResponse, 'status'>[] | undefined
): number {
  return liveListInterval(
    deployments?.some((deployment) =>
      isActiveDeploymentStatus(deployment.status)
    ) ?? false
  )
}
