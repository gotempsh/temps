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
