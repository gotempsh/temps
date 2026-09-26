// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse } from '@/api/client'

const ACTIVE_DEPLOYMENT_STATUSES = new Set(['pending', 'running'])
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
