// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { DeploymentResponse, EnvironmentResponse } from '@/api/client'
import { isActiveDeploymentStatus } from '@/lib/recent-deployments'

export type ProjectDeploymentStatus =
  'Deployed' | 'Deploying' | 'Not deployed' | 'Failed' | 'Degraded'

/**
 * Environment pointers are authoritative; the newest build may not be live.
 * A live project stays "Deployed" while a new build runs, because the live
 * version keeps serving until it is replaced. Only a project with nothing
 * live yet reports its in-progress build as "Deploying".
 *
 * Problems outrank "Deployed": a failed latest deployment reads "Failed" even
 * when an older version is still live (otherwise the failure is invisible
 * from the header), and live containers that are down or crash-looping read
 * "Degraded". A failure wins over degraded containers because it is the
 * newer event and its banner also lists the container problems.
 */
export function projectDeploymentStatus(
  environments:
    Pick<EnvironmentResponse, 'current_deployment_id'>[] | undefined,
  latestDeployment?: Pick<DeploymentResponse, 'status'> | null,
  unhealthyContainerCount = 0
): ProjectDeploymentStatus | undefined {
  if (environments === undefined) return undefined
  const hasLive = environments.some(
    (environment) => environment.current_deployment_id != null
  )
  const latestStatus = latestDeployment?.status
  const building = !!latestStatus && isActiveDeploymentStatus(latestStatus)
  if (!hasLive) {
    if (building) return 'Deploying'
    return latestStatus === 'failed' ? 'Failed' : 'Not deployed'
  }
  if (!building && latestStatus === 'failed') return 'Failed'
  return unhealthyContainerCount > 0 ? 'Degraded' : 'Deployed'
}
