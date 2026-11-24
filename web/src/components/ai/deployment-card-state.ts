// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'

// The deployment states the server writes, grouped the way it groups them for
// operations (`DEPLOYMENT_STATES` in crates/temps-projects/src/services/operations.rs).
const ACTIVE_DEPLOYMENT_STATUSES = new Set([
  'pending',
  'creating',
  'running',
  'in_progress',
  'built',
  'ready',
  'deploying',
])
const READY_DEPLOYMENT_STATUSES = new Set(['completed', 'deployed', 'success'])
const FAILED_DEPLOYMENT_STATUSES = new Set(['failed', 'cancelled'])

export interface DeploymentReference {
  projectId: number
  deploymentId: number | null
  environmentId: number | null
  branch: string | null
  tag: string | null
  commit: string | null
  createdAfterSeconds: number | null
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function parseJsonRecord(value: string | null): Record<string, unknown> {
  if (!value) return {}
  try {
    const parsed: unknown = JSON.parse(value)
    return isRecord(parsed) ? parsed : {}
  } catch {
    return {}
  }
}

function positiveInteger(...values: unknown[]): number | null {
  for (const value of values) {
    if (typeof value === 'number' && Number.isSafeInteger(value) && value > 0) {
      return value
    }
  }
  return null
}

function optionalString(...values: unknown[]): string | null {
  for (const value of values) {
    if (typeof value === 'string' && value.trim()) return value.trim()
  }
  return null
}

/** The action result wins over model-authored proposal parameters because it
 * is the server response from the confirmed operation. */
export function deploymentReference(
  paramsJson: string | null,
  resultJson: string | null,
  createdAt: string | null
): DeploymentReference | null {
  const params = parseJsonRecord(paramsJson)
  const result = parseJsonRecord(resultJson)
  const projectId = positiveInteger(
    result.project_id,
    params.project_id,
    params.id
  )
  if (!projectId) return null

  const parsedCreatedAt = createdAt ? Date.parse(createdAt) : Number.NaN
  return {
    projectId,
    deploymentId: positiveInteger(
      result.deployment_id,
      result.id,
      params.deployment_id
    ),
    environmentId: positiveInteger(
      result.environment_id,
      params.environment_id
    ),
    branch: optionalString(result.branch, params.branch),
    tag: optionalString(result.tag, params.tag),
    commit: optionalString(result.commit, params.commit),
    createdAfterSeconds: Number.isFinite(parsedCreatedAt)
      ? Math.floor(parsedCreatedAt / 1000) - 10
      : null,
  }
}

function commitsMatch(expected: string, actual: string): boolean {
  if (expected === actual) return true
  if (expected.length < 7 || actual.length < 7) return false
  return expected.startsWith(actual) || actual.startsWith(expected)
}

/** Find the deployment created by this trigger without accidentally adopting
 * an older deployment from the same environment. */
export function matchingDeployment(
  deployments: DeploymentResponse[],
  reference: DeploymentReference
): DeploymentResponse | null {
  const matches = deployments.filter((deployment) => {
    if (deployment.project_id !== reference.projectId) return false
    if (reference.deploymentId && deployment.id !== reference.deploymentId) {
      return false
    }
    if (
      reference.environmentId &&
      deployment.environment_id !== reference.environmentId
    ) {
      return false
    }
    if (
      reference.createdAfterSeconds &&
      deployment.created_at < reference.createdAfterSeconds
    ) {
      return false
    }
    if (
      reference.commit &&
      deployment.commit_hash &&
      !commitsMatch(reference.commit, deployment.commit_hash)
    ) {
      return false
    }
    if (
      !reference.commit &&
      reference.branch &&
      deployment.branch &&
      reference.branch !== deployment.branch
    ) {
      return false
    }
    return true
  })

  return matches.sort((a, b) => b.created_at - a.created_at)[0] ?? null
}

export function isActiveDeploymentStatus(status: string): boolean {
  return ACTIVE_DEPLOYMENT_STATUSES.has(status)
}

export function isFailedDeploymentStatus(status: string): boolean {
  return FAILED_DEPLOYMENT_STATUSES.has(status)
}

/** A deployment that finished and can serve its URL. */
export function isReadyDeploymentStatus(status: string): boolean {
  return READY_DEPLOYMENT_STATUSES.has(status)
}

export interface DeploymentStatusPresentation {
  label: string
  className: string
}

const MUTED = 'text-muted-foreground'

/**
 * The label and colour for a deployment state. Only states that have not
 * started yet read "Queued": a finished deployment that a newer one replaced
 * or stopped must never look like it is still waiting, and a state this
 * console does not know is shown as the server named it.
 */
export function deploymentStatusPresentation(
  status: string
): DeploymentStatusPresentation {
  if (isReadyDeploymentStatus(status)) {
    return { label: 'Ready', className: 'text-green-700 dark:text-green-400' }
  }
  if (isFailedDeploymentStatus(status)) {
    return {
      label: status === 'failed' ? 'Failed' : 'Cancelled',
      className: 'text-destructive',
    }
  }
  if (isActiveDeploymentStatus(status)) {
    return {
      label: status === 'pending' ? 'Queued' : 'Deploying',
      className: 'text-blue-600 dark:text-blue-400',
    }
  }
  switch (status) {
    case 'paused':
      return {
        label: 'Paused',
        className: 'text-amber-700 dark:text-amber-400',
      }
    case 'superseded':
      return { label: 'Replaced', className: MUTED }
    case 'stopped':
      return { label: 'Stopped', className: MUTED }
    default:
      return {
        label: status
          ? status.charAt(0).toUpperCase() + status.slice(1).replace(/_/g, ' ')
          : 'Unknown',
        className: MUTED,
      }
  }
}

/**
 * Why a deployment URL is not a link: it will be once the deployment is
 * live, or it is not being served at all.
 */
export function deploymentUrlNote(status: string): string | null {
  if (isReadyDeploymentStatus(status)) return null
  return isActiveDeploymentStatus(status) ? 'once live' : 'not serving'
}

export function deploymentPollingInterval(
  actionStatus: string,
  deploymentStatus: string | null,
  createdAt: string | null,
  nowMs = Date.now()
): number | false {
  if (actionStatus !== 'executed') return false
  if (deploymentStatus && !isActiveDeploymentStatus(deploymentStatus)) {
    return false
  }
  if (!deploymentStatus && createdAt) {
    const ageMs = nowMs - Date.parse(createdAt)
    if (Number.isFinite(ageMs) && ageMs > 2 * 60 * 1000) return false
  }
  return 1500
}
