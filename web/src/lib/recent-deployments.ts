// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'

const ACTIVE_DEPLOYMENT_STATUSES = new Set([
  'pending',
  'running',
  'building',
  'queued',
])

export function recentDeploymentsRefetchInterval(
  deployments: DeploymentResponse[] | undefined
): number | false {
  return deployments?.some((deployment) =>
    ACTIVE_DEPLOYMENT_STATUSES.has(deployment.status)
  )
    ? 2500
    : false
}
