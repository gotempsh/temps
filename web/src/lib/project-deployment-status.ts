// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { DeploymentResponse, EnvironmentResponse } from '@/api/client'
import { isActiveDeploymentStatus } from '@/lib/recent-deployments'

export type ProjectDeploymentStatus = 'Deployed' | 'Deploying' | 'Not deployed'

/**
 * Environment pointers are authoritative; the newest build may not be live.
 * A live project stays "Deployed" while a new build runs, because the live
 * version keeps serving until it is replaced. Only a project with nothing
 * live yet reports its in-progress build as "Deploying".
 */
export function projectDeploymentStatus(
  environments:
    Pick<EnvironmentResponse, 'current_deployment_id'>[] | undefined,
  latestDeployment?: Pick<DeploymentResponse, 'status'> | null
): ProjectDeploymentStatus | undefined {
  if (environments === undefined) return undefined
  if (
    environments.some(
      (environment) => environment.current_deployment_id != null
    )
  ) {
    return 'Deployed'
  }
  return latestDeployment && isActiveDeploymentStatus(latestDeployment.status)
    ? 'Deploying'
    : 'Not deployed'
}
