// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Helpers for locating the deployment created by a pipeline trigger.
 *
 * `trigger-pipeline` only enqueues a job; the deployment row appears later.
 * Listing `per_page: 1` and taking the first row therefore latches onto the
 * previous completed deployment unless we require an id newer than the
 * pre-trigger baseline.
 */

export type DeploymentCandidate = {
  id: number
  commit_hash?: string | null
}

export type SelectTriggeredDeploymentOptions = {
  /** Highest deployment id known before the trigger; accept only id > this. */
  afterId: number
  /** When set, require commit_hash to match (full or abbreviated SHA). */
  commit?: string | null
}

/** True when either value is a prefix of the other (case-insensitive). */
export function commitMatches(
  deploymentHash: string | null | undefined,
  requested: string | null | undefined,
): boolean {
  const want = requested?.trim()
  if (!want) return true
  if (!deploymentHash) return false
  const have = deploymentHash.toLowerCase()
  const needle = want.toLowerCase()
  return have.startsWith(needle) || needle.startsWith(have)
}

/**
 * Pick the newest deployment created after the trigger baseline.
 * `deployments` must be newest-first (API orders by `created_at` DESC).
 */
export function selectTriggeredDeployment(
  deployments: DeploymentCandidate[],
  options: SelectTriggeredDeploymentOptions,
): DeploymentCandidate | null {
  for (const deployment of deployments) {
    if (deployment.id <= options.afterId) continue
    if (!commitMatches(deployment.commit_hash, options.commit)) continue
    return deployment
  }
  return null
}

export type FetchDeploymentsResult = {
  deployments?: DeploymentCandidate[] | null
  error?: unknown
}

export type WaitForTriggeredDeploymentOptions = {
  afterId: number
  commit?: string | null
  maxAttempts?: number
  intervalMs?: number
  sleep?: (ms: number) => Promise<void>
}

export type WaitForTriggeredDeploymentResult =
  | { ok: true; deploymentId: number }
  | { ok: false; error: 'fetch_failed'; cause: unknown }
  | { ok: false; error: 'not_found' }

/**
 * Poll until a deployment newer than `afterId` appears (optionally matching
 * `commit`), or until attempts are exhausted.
 */
export async function waitForTriggeredDeployment(
  fetchDeployments: () => Promise<FetchDeploymentsResult>,
  options: WaitForTriggeredDeploymentOptions,
): Promise<WaitForTriggeredDeploymentResult> {
  const maxAttempts = options.maxAttempts ?? 15
  const intervalMs = options.intervalMs ?? 2000
  const sleep = options.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)))

  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    const { deployments, error } = await fetchDeployments()
    if (error) {
      return { ok: false, error: 'fetch_failed', cause: error }
    }

    const match = selectTriggeredDeployment(deployments ?? [], {
      afterId: options.afterId,
      commit: options.commit,
    })
    if (match) {
      return { ok: true, deploymentId: match.id }
    }

    if (attempt < maxAttempts - 1) {
      await sleep(intervalMs)
    }
  }

  return { ok: false, error: 'not_found' }
}
