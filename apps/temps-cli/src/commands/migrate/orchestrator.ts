// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Migration orchestrator — executes a migration plan against the Temps API.
 *
 * Flow:
 * 1. Run pre-migration verification checks
 * 2. Execute each step in order (create project → services → env vars → git → domains)
 * 3. Run post-migration verification checks
 * 4. Display results
 *
 * The orchestrator ONLY writes to Temps. It never reads from the source platform.
 * All source-platform data is already captured in the MigrationPlan.
 */

import {
  createProject,
  createEnvironmentVariable,
  createService,
  linkServiceToProject,
  createCustomDomain,
  updateGitSettings,
  getEnvironments,
  listGitProviders,
  getProviderConnections,
  listRepositoriesByConnection,
  syncRepositories,
} from '../../api/sdk.gen.js'
import { client, getErrorMessage } from '../../lib/api-client.js'
import type {
  MigrationPlan,
  MigrationResult,
  StepResult,
  MigrationStep,
} from './types.js'
import type { CreatableServiceTypeRoute } from '../../api/types.gen.js'

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

export async function executeMigration(plan: MigrationPlan): Promise<MigrationResult> {
  const startTime = Date.now()
  const stepResults: StepResult[] = []

  let projectId: number | undefined
  let projectSlug: string | undefined
  let environmentId: number | undefined

  // Resolve git connection BEFORE project creation.
  // Project creation triggers an automatic deployment, which needs
  // git_provider_connection_id to clone private repositories.
  // If we wait until the configure-git step, the deployment will already
  // have failed with "git_provider_connection_id is required for private repositories".
  let resolvedConnectionId: number | undefined
  let resolvedConnectionName: string | undefined

  if (plan.project.git) {
    const gitResult = await resolveGitConnection(plan.project.git)
    resolvedConnectionId = gitResult.connectionId
    resolvedConnectionName = gitResult.connectionName
  }

  // Execute each step in order
  for (const step of plan.steps) {
    if (step.skipped) {
      stepResults.push({
        stepId: step.id,
        title: step.title,
        success: true,
        skipped: true,
        message: 'Skipped by user',
        durationMs: 0,
      })
      continue
    }

    const stepStart = Date.now()

    try {
      const result = await executeStep(step, plan, {
        projectId,
        projectSlug,
        environmentId,
        resolvedConnectionId,
        resolvedConnectionName,
      })

      // Update context from step results
      if (result.createdResource?.type === 'project') {
        projectId = result.createdResource.id
        projectSlug = result.createdResource.name

        // After project creation, fetch the default environment
        if (projectId) {
          environmentId = await fetchDefaultEnvironmentId(projectId)
        }
      }

      stepResults.push({
        ...result,
        durationMs: Date.now() - stepStart,
      })
    } catch (err) {
      stepResults.push({
        stepId: step.id,
        title: step.title,
        success: false,
        skipped: false,
        message: err instanceof Error ? err.message : String(err),
        durationMs: Date.now() - stepStart,
      })

      // If project creation fails, abort remaining steps
      if (step.id === 'create-project') {
        break
      }
      // Other step failures are non-fatal — continue with remaining steps
    }
  }

  const allSucceeded = stepResults.every((s) => s.success || s.skipped)

  return {
    success: allSucceeded,
    projectId,
    projectSlug,
    environmentId,
    stepResults,
    durationMs: Date.now() - startTime,
  }
}

// ---------------------------------------------------------------------------
// Step execution router
// ---------------------------------------------------------------------------

interface StepContext {
  projectId?: number
  projectSlug?: string
  environmentId?: number
  /** Resolved git connection ID — set before project creation to avoid race with auto-deploy */
  resolvedConnectionId?: number
  resolvedConnectionName?: string
}

async function executeStep(
  step: MigrationStep,
  plan: MigrationPlan,
  ctx: StepContext
): Promise<Omit<StepResult, 'durationMs'>> {
  switch (step.id) {
    case 'create-project':
      return executeCreateProject(plan, ctx)

    case 'set-env-vars':
      return executeSetEnvVars(plan, ctx)

    case 'configure-git':
      return executeConfigureGit(plan, ctx)

    default:
      if (step.id.startsWith('service-')) {
        const serviceName = step.id.replace('service-', '')
        const svc = plan.services.find((s) => s.name === serviceName)
        if (!svc) throw new Error(`Service "${serviceName}" not found in plan`)
        return executeCreateService(svc, ctx)
      }

      if (step.id.startsWith('domain-')) {
        const domainName = step.id.replace('domain-', '')
        const domain = plan.domains.find((d) => d.domain === domainName)
        if (!domain) throw new Error(`Domain "${domainName}" not found in plan`)
        return executeAddDomain(domain, ctx)
      }

      throw new Error(`Unknown step ID: ${step.id}`)
  }
}

// ---------------------------------------------------------------------------
// Step implementations
// ---------------------------------------------------------------------------

async function executeCreateProject(
  plan: MigrationPlan,
  ctx: StepContext
): Promise<Omit<StepResult, 'durationMs'>> {
  const { data, error } = await createProject({
    client,
    body: {
      name: plan.project.name,
      preset: plan.project.preset,
      directory: plan.project.directory,
      main_branch: plan.project.mainBranch,
      build_command: plan.project.buildCommand ?? null,
      install_command: plan.project.installCommand ?? null,
      output_dir: plan.project.outputDir ?? null,
      exposed_port: plan.project.exposedPort ?? null,
      repo_name: plan.project.git?.repo ?? null,
      repo_owner: plan.project.git?.owner ?? null,
      git_provider_connection_id: ctx.resolvedConnectionId ?? null,
      storage_service_ids: [],
    },
  })

  if (error) {
    throw new Error(`Failed to create project: ${getErrorMessage(error)}`)
  }

  const project = data as { id: number; name: string; slug?: string }

  return {
    stepId: 'create-project',
    title: `Create project "${plan.project.name}"`,
    success: true,
    skipped: false,
    message: `Project created (ID: ${project.id})`,
    createdResource: {
      type: 'project',
      id: project.id,
      name: project.name ?? plan.project.name,
    },
  }
}

async function executeSetEnvVars(
  plan: MigrationPlan,
  ctx: StepContext
): Promise<Omit<StepResult, 'durationMs'>> {
  if (!ctx.projectId) throw new Error('Project must be created before setting env vars')
  if (!ctx.environmentId) throw new Error('No environment found for project')

  const activeEnvVars = plan.envVars.filter((ev) => !ev.skip)
  let successCount = 0
  let failCount = 0
  const errors: string[] = []

  for (const ev of activeEnvVars) {
    try {
      const { error } = await createEnvironmentVariable({
        client,
        path: { project_id: ctx.projectId },
        body: {
          key: ev.key,
          value: ev.value,
          environment_ids: [ctx.environmentId],
          include_in_preview: true,
        },
      })

      if (error) {
        failCount++
        errors.push(`${ev.key}: ${getErrorMessage(error)}`)
      } else {
        successCount++
      }
    } catch (err) {
      failCount++
      errors.push(`${ev.key}: ${err instanceof Error ? err.message : String(err)}`)
    }
  }

  const success = failCount === 0
  const message = success
    ? `Set ${successCount} environment variable(s)`
    : `Set ${successCount}/${activeEnvVars.length} env vars (${failCount} failed: ${errors.slice(0, 3).join('; ')}${errors.length > 3 ? '...' : ''})`

  return {
    stepId: 'set-env-vars',
    title: `Set ${activeEnvVars.length} environment variable(s)`,
    success,
    skipped: false,
    message,
  }
}

async function executeCreateService(
  svc: { name: string; type: string; version?: string; action: string; envVarKeys: string[] },
  ctx: StepContext
): Promise<Omit<StepResult, 'durationMs'>> {
  if (!ctx.projectId) throw new Error('Project must be created before creating services')

  if (svc.action === 'skip') {
    return {
      stepId: `service-${svc.name}`,
      title: `Skip service "${svc.name}"`,
      success: true,
      skipped: true,
      message: 'Service skipped',
    }
  }

  // Map service type to Temps ServiceTypeRoute
  const serviceTypeMap: Record<string, CreatableServiceTypeRoute> = {
    postgres: 'postgres',
    redis: 'redis',
    mongodb: 'mongodb',
    s3: 's3',
  }

  const serviceType = serviceTypeMap[svc.type]
  if (!serviceType) {
    return {
      stepId: `service-${svc.name}`,
      title: `Create service "${svc.name}"`,
      success: false,
      skipped: false,
      message: `Unsupported service type: ${svc.type}. Supported: ${Object.keys(serviceTypeMap).join(', ')}`,
    }
  }

  // Create the service
  const { data: serviceData, error: serviceError } = await createService({
    client,
    body: {
      name: svc.name,
      service_type: serviceType,
      version: svc.version ?? null,
      parameters: {},
    },
  })

  if (serviceError) {
    throw new Error(`Failed to create service: ${getErrorMessage(serviceError)}`)
  }

  const createdService = serviceData as { id: number; name: string }

  // Link the service to the project
  try {
    await linkServiceToProject({
      client,
      path: { id: createdService.id },
      body: { project_id: ctx.projectId },
    })
  } catch (linkErr) {
    // Non-fatal — service was created but linking failed
    return {
      stepId: `service-${svc.name}`,
      title: `Create service "${svc.name}"`,
      success: true, // Service was created
      skipped: false,
      message: `Service created (ID: ${createdService.id}) but linking to project failed: ${linkErr instanceof Error ? linkErr.message : String(linkErr)}`,
      createdResource: {
        type: 'service',
        id: createdService.id,
        name: createdService.name ?? svc.name,
      },
    }
  }

  return {
    stepId: `service-${svc.name}`,
    title: `Create service "${svc.name}"`,
    success: true,
    skipped: false,
    message: `Service created and linked to project (ID: ${createdService.id})`,
    createdResource: {
      type: 'service',
      id: createdService.id,
      name: createdService.name ?? svc.name,
    },
  }
}

async function executeConfigureGit(
  plan: MigrationPlan,
  ctx: StepContext
): Promise<Omit<StepResult, 'durationMs'>> {
  if (!ctx.projectId) throw new Error('Project must be created before configuring git')
  if (!plan.project.git) throw new Error('No git info in plan')

  const git = plan.project.git
  const repoFullName = `${git.owner}/${git.repo}`
  const stepTitle = `Configure git: ${repoFullName}`

  // Use the pre-resolved connection from executeMigration (resolved before project creation).
  // If not available, resolve now as a fallback.
  let connectionId = ctx.resolvedConnectionId
  let connectionName = ctx.resolvedConnectionName

  if (!connectionId) {
    const result = await resolveGitConnection(git)
    connectionId = result.connectionId
    connectionName = result.connectionName

    if (!connectionId) {
      return {
        stepId: 'configure-git',
        title: stepTitle,
        success: false,
        skipped: false,
        message: result.errorMessage ?? `No ${git.provider} connection found in Temps.`,
      }
    }
  }

  // Call updateGitSettings to ensure everything is persisted and validated
  // (branch existence check, connection active check, etc.)
  const { error } = await updateGitSettings({
    client,
    path: { project_id: ctx.projectId },
    body: {
      repo_owner: git.owner,
      repo_name: git.repo,
      main_branch: plan.project.mainBranch,
      directory: plan.project.directory,
      git_provider_connection_id: connectionId,
    },
  })

  if (error) {
    return {
      stepId: 'configure-git',
      title: stepTitle,
      success: false,
      skipped: false,
      message: `Failed to configure git: ${getErrorMessage(error)}`,
    }
  }

  return {
    stepId: 'configure-git',
    title: stepTitle,
    success: true,
    skipped: false,
    message: `Git configured: ${repoFullName} via "${connectionName}" (branch: ${plan.project.mainBranch})`,
  }
}

/**
 * Search for a repository on a specific git connection.
 * Returns true if the repo is found.
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
 * This handles the case where the repo exists but hasn't been synced yet.
 */
async function syncAndFindRepo(
  connectionId: number,
  owner: string,
  repo: string
): Promise<boolean> {
  try {
    // Trigger a sync
    await syncRepositories({
      client,
      path: { connection_id: connectionId },
    })

    // Wait a moment for sync to complete
    await new Promise((resolve) => setTimeout(resolve, 1000))

    // Search again
    return findRepoOnConnection(connectionId, owner, repo)
  } catch {
    return false
  }
}

async function executeAddDomain(
  domain: { domain: string; redirectTo?: string; statusCode?: number },
  ctx: StepContext
): Promise<Omit<StepResult, 'durationMs'>> {
  if (!ctx.projectId) throw new Error('Project must be created before adding domains')
  if (!ctx.environmentId) throw new Error('No environment found for project')

  const { data, error } = await createCustomDomain({
    client,
    path: { project_id: ctx.projectId },
    body: {
      domain: domain.domain,
      environment_id: ctx.environmentId,
      redirect_to: domain.redirectTo ?? null,
      status_code: domain.statusCode ?? null,
    },
  })

  if (error) {
    throw new Error(`Failed to add domain: ${getErrorMessage(error)}`)
  }

  return {
    stepId: `domain-${domain.domain}`,
    title: `Add domain "${domain.domain}"`,
    success: true,
    skipped: false,
    message: `Domain "${domain.domain}" added`,
    createdResource: {
      type: 'domain',
      id: (data as { id?: number })?.id ?? 0,
      name: domain.domain,
    },
  }
}

// ---------------------------------------------------------------------------
// Git connection resolution helpers
// ---------------------------------------------------------------------------

interface GitConnectionResult {
  connectionId?: number
  connectionName?: string
  errorMessage?: string
}

/**
 * Resolve which git connection has access to the target repository.
 *
 * This is called BEFORE project creation so that `git_provider_connection_id`
 * can be passed in the `createProject` body. Without it, the automatic
 * deployment triggered by project creation fails for private repos with:
 * "git_provider_connection_id is required for private repositories"
 */
async function resolveGitConnection(
  git: import('./types.js').GitInfo
): Promise<GitConnectionResult> {
  const targetProvider = git.provider.toLowerCase()
  const repoFullName = `${git.owner}/${git.repo}`

  // Step 1: Get connections for the matching provider type
  let matchingConnections: Array<{ id: number; account_name: string; account_type: string; is_active: boolean }>

  try {
    const connections = await getConnectionsByProviderType(targetProvider)
    matchingConnections = connections.filter((c) => c.is_active)
    if (matchingConnections.length === 0) {
      matchingConnections = connections
    }
  } catch {
    return {
      errorMessage: 'Could not list git connections. Connect a git provider in Temps settings.',
    }
  }

  if (matchingConnections.length === 0) {
    return {
      errorMessage: `No ${git.provider} connection found in Temps. Go to Temps settings -> Git Providers to connect your ${git.provider} account.`,
    }
  }

  // Step 2: Search for the repo on each connection
  for (const conn of matchingConnections) {
    if (await findRepoOnConnection(conn.id, git.owner, git.repo)) {
      return { connectionId: conn.id, connectionName: conn.account_name }
    }
  }

  // Step 3: Sync and retry
  for (const conn of matchingConnections) {
    if (await syncAndFindRepo(conn.id, git.owner, git.repo)) {
      return { connectionId: conn.id, connectionName: conn.account_name }
    }
  }

  // Step 4: Not found on any connection
  const connNames = matchingConnections.map((c) => c.account_name).join(', ')
  return {
    errorMessage: [
      `Repository "${repoFullName}" not accessible through any ${git.provider} connection (tried: ${connNames}).`,
      `Possible causes:`,
      `  - The GitHub/GitLab App installation does not include this repository`,
      `  - The repository owner "${git.owner}" is not in the connected account's scope`,
      `Action: Go to Temps settings -> Git Providers -> update your ${git.provider} App installation to include "${repoFullName}".`,
    ].join('\n'),
  }
}

/**
 * Get all connections for a given provider type (e.g. "github", "gitlab").
 *
 * The `provider_type` lives on the `git_providers` table, NOT on connections.
 * Connections only have `provider_id` (FK) and `account_type` ("User"/"Organization").
 * So we: list providers → find matching provider → get its connections.
 */
async function getConnectionsByProviderType(
  providerType: string
): Promise<Array<{ id: number; account_name: string; account_type: string; is_active: boolean }>> {
  // 1. List all git providers to find the one matching the target type
  const { data: providers } = await listGitProviders({ client })
  if (!providers || !Array.isArray(providers)) return []

  const matchingProvider = providers.find(
    (p) => p.provider_type?.toLowerCase() === providerType.toLowerCase()
  )
  if (!matchingProvider) return []

  // 2. Get connections for this provider
  const { data: connections } = await getProviderConnections({
    client,
    path: { provider_id: matchingProvider.id },
  })
  if (!connections || !Array.isArray(connections)) return []

  return connections
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function fetchDefaultEnvironmentId(projectId: number): Promise<number | undefined> {
  try {
    const { data } = await getEnvironments({ client, path: { project_id: projectId } })
    const envs = Array.isArray(data) ? data : []

    if (envs.length === 0) return undefined

    // Prefer "production" environment, otherwise first one
    const production = envs.find(
      (e: { name?: string }) => e.name?.toLowerCase() === 'production'
    )
    const env = production ?? envs[0]
    return (env as { id: number }).id
  } catch {
    return undefined
  }
}
