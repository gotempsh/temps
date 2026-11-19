// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * A container's nameservers are fixed when it is created, so turning Cluster
 * DNS on reaches new containers only. Applications already running keep
 * Docker's embedded DNS and cannot resolve `*.temps.local` names until they
 * are redeployed. The server lists them in `GET /cluster/dns/status`; these
 * helpers describe and redeploy them.
 */

import type {
  ClusterDnsStatusResponse,
  DeploymentWithoutResolver,
  ResolverCoverageHost,
} from '@/api/client'
import { getProject } from '@/api/client/sdk.gen'
import { redeployDeployment, sdkRedeployApi } from '@/lib/service-link-redeploy'

export type CoverageApi = typeof sdkRedeployApi & {
  getProject: typeof getProject
}

export const sdkCoverageApi: CoverageApi = { ...sdkRedeployApi, getProject }

/** The applications of one project that run without the resolver. */
export function withoutResolverForProject(
  status: ClusterDnsStatusResponse | undefined,
  projectId: number
): DeploymentWithoutResolver[] {
  if (!status?.cluster_dns_enabled) return []
  return (status.deployments_without_resolver ?? []).filter(
    (entry) => entry.project_id === projectId
  )
}

/** "the control plane", "worker-a", "worker-a and worker-b", … */
export function hostsLabel(hosts: ResolverCoverageHost[]): string {
  const names = hosts.map((host) =>
    host.node_id == null ? 'the control plane' : host.node_name
  )
  if (names.length <= 1) return names[0] ?? 'an unknown host'
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
}

/** Why the listed environments need a redeploy, in one sentence each. */
export function staleBannerMessage(environments: string[]): string {
  const one = environments.length === 1
  const names = one
    ? `The ${environments[0]} environment was`
    : `The ${environments.slice(0, -1).join(', ')} and ${environments[environments.length - 1]} environments were`
  return (
    `${names} started before Cluster DNS was on, so ${one ? 'it cannot' : 'they cannot'} ` +
    `reach other services by their *.temps.local names, such as a database's internal ` +
    `hostname. Redeploy ${one ? 'it' : 'them'} to fix this.`
  )
}

/**
 * Redeploy an application's current deployment from the same source, so its
 * new containers get the resolver. Resolves to why it could not be
 * redeployed, or `undefined` once it was started.
 */
export async function redeployWithoutResolver(
  entry: DeploymentWithoutResolver,
  api: CoverageApi = sdkCoverageApi
): Promise<string | undefined> {
  const [{ data: project }, { data: deployment }] = await Promise.all([
    api.getProject({ path: { id: entry.project_id }, throwOnError: true }),
    api.getDeployment({
      path: {
        project_id: entry.project_id,
        deployment_id: entry.deployment_id,
      },
      throwOnError: true,
    }),
  ])
  return redeployDeployment(
    api,
    entry.project_id,
    project.source_type,
    deployment
  )
}
