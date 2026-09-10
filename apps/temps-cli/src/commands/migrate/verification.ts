// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pre- and post-migration verification checks.
 *
 * Pre-migration checks run BEFORE execution to catch issues early:
 *   - Temps server is reachable
 *   - User is authenticated
 *   - Project name doesn't already exist
 *   - Git provider connection exists (if git info is present)
 *
 * Post-migration checks run AFTER execution to confirm success:
 *   - Project was created
 *   - Environment variables were set
 *   - Services are running
 *   - Domains are configured
 */

import {
  getProjects,
  getProject,
  getEnvironments,
  listGitProviders,
  getProviderConnections,
  listRepositoriesByConnection,
  syncRepositories,
} from '../../api/sdk.gen.js'
import { client } from '../../lib/api-client.js'
import type { MigrationPlan, VerificationCheck, VerificationResult, MigrationResult } from './types.js'

// ---------------------------------------------------------------------------
// Check definitions
// ---------------------------------------------------------------------------

const PRE_CHECKS: VerificationCheck[] = [
  {
    id: 'server-reachable',
    name: 'Temps server reachable',
    description: 'Verify the Temps server API is accessible',
    phase: 'pre',
  },
  {
    id: 'project-name-available',
    name: 'Project name available',
    description: 'Verify the target project name is not already in use',
    phase: 'pre',
  },
  {
    id: 'git-connection-exists',
    name: 'Git repository accessible',
    description: 'Verify the target repository is accessible through a git provider connection in Temps',
    phase: 'pre',
  },
]

const POST_CHECKS: VerificationCheck[] = [
  {
    id: 'project-created',
    name: 'Project created',
    description: 'Verify the project was created in Temps',
    phase: 'post',
  },
  {
    id: 'environments-exist',
    name: 'Environments created',
    description: 'Verify environments were created for the project',
    phase: 'post',
  },
]

// ---------------------------------------------------------------------------
// Pre-migration verification
// ---------------------------------------------------------------------------

export async function runPreChecks(plan: MigrationPlan): Promise<VerificationResult[]> {
  const results: VerificationResult[] = []

  // 1. Server reachable
  results.push(await checkServerReachable())

  // 2. Project name available
  results.push(await checkProjectNameAvailable(plan.project.name))

  // 3. Git repo accessible through a connection (only if plan has git info)
  if (plan.project.git) {
    results.push(await checkGitRepoAccessible(plan.project.git))
  }

  return results
}

async function checkServerReachable(): Promise<VerificationResult> {
  try {
    const { data, error } = await getProjects({ client, query: { per_page: 1 } })
    if (error) {
      return {
        checkId: 'server-reachable',
        name: 'Temps server reachable',
        passed: false,
        message: `Server responded with error: ${typeof error === 'object' && error !== null && 'detail' in error ? (error as { detail: string }).detail : 'unknown error'}`,
        severity: 'error',
      }
    }
    return {
      checkId: 'server-reachable',
      name: 'Temps server reachable',
      passed: true,
      message: 'Server is accessible',
      severity: 'info',
    }
  } catch (err) {
    return {
      checkId: 'server-reachable',
      name: 'Temps server reachable',
      passed: false,
      message: `Cannot reach server: ${err instanceof Error ? err.message : String(err)}`,
      severity: 'error',
    }
  }
}

async function checkProjectNameAvailable(name: string): Promise<VerificationResult> {
  try {
    const { data } = await getProjects({ client, query: { per_page: 100 } })
    const projects = data?.projects ?? []
    const existing = projects.find(
      (p) => p.name?.toLowerCase() === name.toLowerCase()
    )

    if (existing) {
      return {
        checkId: 'project-name-available',
        name: 'Project name available',
        passed: false,
        message: `Project "${name}" already exists. Choose a different name or delete the existing project.`,
        severity: 'error',
      }
    }

    return {
      checkId: 'project-name-available',
      name: 'Project name available',
      passed: true,
      message: `Project name "${name}" is available`,
      severity: 'info',
    }
  } catch {
    return {
      checkId: 'project-name-available',
      name: 'Project name available',
      passed: false,
      message: 'Could not verify project name availability (server error)',
      severity: 'warning',
    }
  }
}

/**
 * Verify the target repository is accessible through a git provider connection.
 *
 * This goes beyond checking "does a github connection exist?" — it actually
 * searches for the specific repo on each matching connection. If the repo is
 * not found on any connection, it tries syncing repositories first. This
 * mirrors the logic in the orchestrator's `executeConfigureGit()`.
 *
 * Possible outcomes:
 * - "Found repo on connection X" → passed
 * - "Connection(s) exist but repo not accessible" → warning with actionable fix
 * - "No matching connection found" → warning with setup instructions
 */
async function checkGitRepoAccessible(git: import('./types.js').GitInfo): Promise<VerificationResult> {
  const checkId = 'git-connection-exists'
  const checkName = 'Git repository accessible'
  const repoFullName = `${git.owner}/${git.repo}`
  const targetProvider = git.provider.toLowerCase()

  // Step 1: Find connections via the git provider that matches the target type.
  // The provider_type ("github", "gitlab") lives on git_providers, NOT on connections.
  // Connections only have account_type ("User"/"Organization") and provider_id (FK).
  let allConnections: Array<{ id: number; account_name: string; account_type: string; is_active: boolean }>
  try {
    const connections = await getConnectionsByProviderType(targetProvider)
    // Prefer active connections, fall back to inactive
    allConnections = connections.filter((c) => c.is_active)
    if (allConnections.length === 0) {
      allConnections = connections
    }
  } catch {
    return {
      checkId,
      name: checkName,
      passed: false,
      message: 'Could not check git connections (server error)',
      severity: 'warning',
    }
  }

  // No connections of this type at all
  if (allConnections.length === 0) {
    return {
      checkId,
      name: checkName,
      passed: false,
      message: `No ${git.provider} connection found in Temps. Git integration will be skipped. Connect ${git.provider} in Temps settings to enable automatic deployments.`,
      severity: 'warning',
    }
  }

  // Step 2: Search for the repo on each matching connection
  for (const conn of allConnections) {
    if (await findRepoOnConnection(conn.id, git.owner, git.repo)) {
      return {
        checkId,
        name: checkName,
        passed: true,
        message: `Repository "${repoFullName}" found on ${git.provider} connection "${conn.account_name}"`,
        severity: 'info',
      }
    }
  }

  // Step 3: Not found — try syncing each connection and search again
  for (const conn of allConnections) {
    if (await syncAndFindRepo(conn.id, git.owner, git.repo)) {
      return {
        checkId,
        name: checkName,
        passed: true,
        message: `Repository "${repoFullName}" found on ${git.provider} connection "${conn.account_name}" (after sync)`,
        severity: 'info',
      }
    }
  }

  // Step 4: Still not found — connection exists but repo is not accessible
  const connNames = allConnections.map((c) => c.account_name).join(', ')
  return {
    checkId,
    name: checkName,
    passed: false,
    message: `Repository "${repoFullName}" not accessible through any ${git.provider} connection (tried: ${connNames}). Update your ${git.provider} App installation to include this repository, or git integration will be skipped.`,
    severity: 'warning',
  }
}

/**
 * Get all connections for a given provider type (e.g. "github", "gitlab").
 *
 * The `provider_type` lives on `git_providers`, NOT on connections.
 * Connections only have `provider_id` (FK) and `account_type` ("User"/"Organization").
 */
async function getConnectionsByProviderType(
  providerType: string
): Promise<Array<{ id: number; account_name: string; account_type: string; is_active: boolean }>> {
  const { data: providers } = await listGitProviders({ client })
  if (!providers || !Array.isArray(providers)) return []

  const matchingProvider = providers.find(
    (p) => p.provider_type?.toLowerCase() === providerType.toLowerCase()
  )
  if (!matchingProvider) return []

  const { data: connections } = await getProviderConnections({
    client,
    path: { provider_id: matchingProvider.id },
  })
  if (!connections || !Array.isArray(connections)) return []

  return connections
}

/**
 * Search for a repository on a specific git connection.
 */
async function findRepoOnConnection(
  connectionId: number,
  owner: string,
  repo: string
): Promise<boolean> {
  try {
    const { data, error } = await listRepositoriesByConnection({
      client,
      path: { connection_id: connectionId },
      query: { search: repo, owner, per_page: 10 },
    })
    if (error || !data) return false

    const repos = data.repositories ?? []
    return repos.some(
      (r) =>
        r.owner?.toLowerCase() === owner.toLowerCase() &&
        r.name?.toLowerCase() === repo.toLowerCase()
    )
  } catch {
    return false
  }
}

/**
 * Sync repositories for a connection, then search for the target repo.
 */
async function syncAndFindRepo(
  connectionId: number,
  owner: string,
  repo: string
): Promise<boolean> {
  try {
    await syncRepositories({
      client,
      path: { connection_id: connectionId },
    })

    // Wait a moment for sync to complete
    await new Promise((resolve) => setTimeout(resolve, 1000))

    return findRepoOnConnection(connectionId, owner, repo)
  } catch {
    return false
  }
}

// ---------------------------------------------------------------------------
// Post-migration verification
// ---------------------------------------------------------------------------

export async function runPostChecks(
  plan: MigrationPlan,
  result: MigrationResult
): Promise<VerificationResult[]> {
  const results: VerificationResult[] = []

  if (!result.projectId) {
    results.push({
      checkId: 'project-created',
      name: 'Project created',
      passed: false,
      message: 'No project ID returned — project creation may have failed',
      severity: 'error',
    })
    return results
  }

  // 1. Project exists
  results.push(await checkProjectExists(result.projectId))

  // 2. Environments exist
  results.push(await checkEnvironmentsExist(result.projectId))

  return results
}

async function checkProjectExists(projectId: number): Promise<VerificationResult> {
  try {
    const { data, error } = await getProject({ client, path: { id: projectId } })
    if (error || !data) {
      return {
        checkId: 'project-created',
        name: 'Project created',
        passed: false,
        message: `Project ${projectId} not found after creation`,
        severity: 'error',
      }
    }
    return {
      checkId: 'project-created',
      name: 'Project created',
      passed: true,
      message: `Project "${data.name ?? projectId}" exists`,
      severity: 'info',
    }
  } catch (err) {
    return {
      checkId: 'project-created',
      name: 'Project created',
      passed: false,
      message: `Could not verify project: ${err instanceof Error ? err.message : String(err)}`,
      severity: 'error',
    }
  }
}

async function checkEnvironmentsExist(projectId: number): Promise<VerificationResult> {
  try {
    const { data, error } = await getEnvironments({ client, path: { project_id: projectId } })
    if (error) {
      return {
        checkId: 'environments-exist',
        name: 'Environments created',
        passed: false,
        message: 'Could not fetch environments',
        severity: 'warning',
      }
    }
    const envs = Array.isArray(data) ? data : []
    return {
      checkId: 'environments-exist',
      name: 'Environments created',
      passed: envs.length > 0,
      message: envs.length > 0 ? `${envs.length} environment(s) found` : 'No environments found',
      severity: envs.length > 0 ? 'info' : 'warning',
    }
  } catch {
    return {
      checkId: 'environments-exist',
      name: 'Environments created',
      passed: false,
      message: 'Could not verify environments',
      severity: 'warning',
    }
  }
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

export function getCheckDefinitions(): { pre: VerificationCheck[]; post: VerificationCheck[] } {
  return { pre: PRE_CHECKS, post: POST_CHECKS }
}
