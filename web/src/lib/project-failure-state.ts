// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers behind the project overview's Failed / Degraded states: which
 * environment's containers to watch, which of them are unhealthy, and which
 * deployment a failed one can roll back to. Kept free of React so the rules
 * are unit-tested on their own.
 */

import type {
  ContainerInfoResponse,
  DeploymentResponse,
  EnvironmentResponse,
} from '@/api/client'

/**
 * Deployment statuses the rollback endpoint accepts as a target: the live
 * one (`completed`/`deployed`) and older successful ones that a newer
 * deployment superseded (`stopped`).
 */
const ROLLBACK_TARGET_STATUSES = new Set(['completed', 'deployed', 'stopped'])

/** Statuses of a container that should be serving but is not. */
const DOWN_CONTAINER_STATUSES = new Set(['exited', 'dead', 'stopped'])

/**
 * How recently a container must have (re)started, after at least one Docker
 * restart, to count as crash-looping. Docker's restart count never resets,
 * so an old restart followed by hours of uptime is not a current problem.
 */
export const RECENT_RESTART_WINDOW_MS = 15 * 60 * 1000

export type ContainerHealthIssue = {
  /** Docker container ID, the identifier the container API takes. */
  containerId: string
  containerName: string
  kind: 'down' | 'restarting'
  /** Human-readable reason, e.g. "Exited: OOMKilled" or "Restarted 3 times". */
  detail: string
}

export function isRollbackTargetStatus(status: string): boolean {
  return ROLLBACK_TARGET_STATUSES.has(status)
}

/**
 * The environment whose containers decide "Degraded": the first non-preview
 * environment with something live, falling back to any live environment.
 * Previews are usually idle or short-lived, so a crash there should not mark
 * the whole project degraded when production is fine.
 */
export function healthEnvironment(
  environments: EnvironmentResponse[] | undefined
): EnvironmentResponse | undefined {
  const live = (environments ?? []).filter(
    (environment) => environment.current_deployment_id != null
  )
  return live.find((environment) => !environment.is_preview) ?? live[0]
}

/**
 * Live containers that are down or crash-looping. A sleeping (on-demand)
 * environment stops its containers on purpose, so it reports nothing.
 */
export function containerHealthIssues(
  containers: ContainerInfoResponse[] | undefined,
  options: { sleeping?: boolean; now?: number } = {}
): ContainerHealthIssue[] {
  if (options.sleeping) return []
  const now = options.now ?? Date.now()
  const issues: ContainerHealthIssue[] = []
  for (const container of containers ?? []) {
    const name = container.service_name || container.container_name
    if (DOWN_CONTAINER_STATUSES.has(container.status)) {
      const reason = container.oom_killed
        ? 'OOMKilled'
        : (container.exit_reason ?? container.error_message)
      issues.push({
        containerId: container.container_id,
        containerName: name,
        kind: 'down',
        detail: reason
          ? `${capitalize(container.status)}: ${reason}`
          : capitalize(container.status),
      })
      continue
    }
    const restarts = container.restart_count ?? 0
    const startedAt = container.started_at
      ? Date.parse(container.started_at)
      : Number.NaN
    if (
      restarts > 0 &&
      Number.isFinite(startedAt) &&
      now - startedAt < RECENT_RESTART_WINDOW_MS
    ) {
      issues.push({
        containerId: container.container_id,
        containerName: name,
        kind: 'restarting',
        detail: `Restarted ${restarts} time${restarts === 1 ? '' : 's'}${
          container.oom_killed ? ' (last exit: OOMKilled)' : ''
        }`,
      })
    }
  }
  return issues
}

/**
 * The newest deployment in the failed deployment's environment that the
 * rollback endpoint accepts, created before the failure. `deployments` may
 * come in any order and may include other environments.
 */
export function lastSuccessfulDeployment(
  deployments: DeploymentResponse[] | undefined,
  failed: Pick<DeploymentResponse, 'id' | 'environment_id' | 'created_at'>
): DeploymentResponse | undefined {
  let best: DeploymentResponse | undefined
  for (const deployment of deployments ?? []) {
    if (
      deployment.id === failed.id ||
      deployment.environment_id !== failed.environment_id ||
      deployment.created_at > failed.created_at ||
      !isRollbackTargetStatus(deployment.status)
    ) {
      continue
    }
    if (!best || deployment.created_at > best.created_at) best = deployment
  }
  return best
}

function capitalize(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1)
}
