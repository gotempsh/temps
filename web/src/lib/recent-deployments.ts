// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'

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
