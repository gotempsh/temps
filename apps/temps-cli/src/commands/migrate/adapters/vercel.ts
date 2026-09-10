// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Vercel platform adapter.
 *
 * Vercel REST API:
 *   Base URL: https://api.vercel.com
 *   Auth: Bearer token
 *   Team scoping: ?teamId=<id> query parameter
 *
 * Key endpoints used:
 *   GET /v9/projects           — list projects
 *   GET /v9/projects/:id       — project detail
 *   GET /v10/projects/:id/env?decrypt=true — env vars (decrypted)
 *   GET /v13/deployments       — recent deployments (for git info)
 *   GET /v2/user               — validate token (who am i)
 *   GET /v2/teams              — list teams (for teamId discovery)
 */

import type {
  PlatformCredentials,
  DiscoveredProject,
  ProjectSnapshot,
  MigrationPlan,
  EnvVar,
  ServiceSnapshot,
  DomainSnapshot,
  GitInfo,
  BuildInfo,
  EnvVarPlan,
  ServicePlan,
  DomainPlan,
  UnsupportedFeature,
} from '../types.js'
import type { PlatformAdapter, CredentialValidation } from './base.js'
import {
  isLikelySecret,
  detectServiceTypeFromKey,
  detectServiceTypeFromUrl,
  inferPreset,
  buildSteps,
  buildSummary,
} from './base.js'

const VERCEL_API = 'https://api.vercel.com'

// ---------------------------------------------------------------------------
// Vercel API response types (subset we care about)
// ---------------------------------------------------------------------------

interface VercelUser {
  id: string
  email: string
  name?: string
  username?: string
}

interface VercelTeam {
  id: string
  name: string
  slug: string
}

interface VercelProject {
  id: string
  name: string
  framework?: string | null
  updatedAt?: number
  link?: {
    type?: string
    repo?: string
    repoOwner?: string
    org?: string
    repoId?: number
    productionBranch?: string
    gitCredentialId?: string
    sourceless?: boolean
    createdAt?: number
    updatedAt?: number
    deployHooks?: unknown[]
  }
  buildCommand?: string | null
  installCommand?: string | null
  outputDirectory?: string | null
  devCommand?: string | null
  rootDirectory?: string | null
  latestDeployments?: VercelDeployment[]
  targets?: Record<string, VercelDeployment>
  env?: VercelEnvVar[]
  /** Custom domains assigned at the project level */
  alias?: { domain: string; redirect?: string | null; redirectStatusCode?: number }[]
}

interface VercelDeployment {
  uid: string
  url: string
  state: string
  meta?: {
    githubCommitSha?: string
    githubCommitRef?: string
    gitlabCommitSha?: string
    gitlabCommitRef?: string
    bitbucketCommitSha?: string
    bitbucketCommitRef?: string
  }
}

interface VercelEnvVar {
  id: string
  key: string
  value?: string
  type: 'plain' | 'secret' | 'encrypted' | 'system' | 'sensitive'
  target?: ('production' | 'preview' | 'development')[]
  gitBranch?: string | null
  decrypted?: boolean
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

async function vercelFetch<T>(
  path: string,
  creds: PlatformCredentials
): Promise<T> {
  const url = new URL(path, VERCEL_API)
  if (creds.teamId) {
    url.searchParams.set('teamId', creds.teamId)
  }

  const res = await fetch(url.toString(), {
    headers: {
      Authorization: `Bearer ${creds.token}`,
      'Content-Type': 'application/json',
    },
  })

  if (!res.ok) {
    const body = await res.text()
    throw new Error(`Vercel API ${res.status}: ${body}`)
  }

  return res.json() as Promise<T>
}

// ---------------------------------------------------------------------------
// Adapter implementation
// ---------------------------------------------------------------------------

export class VercelAdapter implements PlatformAdapter {
  readonly platformId = 'vercel' as const

  async validateCredentials(creds: PlatformCredentials): Promise<CredentialValidation> {
    try {
      const user = await vercelFetch<{ user: VercelUser }>('/v2/user', creds)
      let accountName = user.user.name || user.user.username || user.user.email

      // If teamId is set, also validate the team
      if (creds.teamId) {
        try {
          const teams = await vercelFetch<{ teams: VercelTeam[] }>('/v2/teams', creds)
          const team = teams.teams.find((t) => t.id === creds.teamId || t.slug === creds.teamId)
          if (team) {
            accountName = `${accountName} (team: ${team.name})`
          } else {
            return {
              valid: false,
              message: `Team "${creds.teamId}" not found. Available teams: ${teams.teams.map((t) => t.slug).join(', ')}`,
            }
          }
        } catch {
          // Team listing failed — still valid if personal account
        }
      }

      return {
        valid: true,
        message: `Authenticated as ${accountName}`,
        accountName,
      }
    } catch (err) {
      return {
        valid: false,
        message: `Authentication failed: ${err instanceof Error ? err.message : String(err)}`,
      }
    }
  }

  async discoverProjects(creds: PlatformCredentials): Promise<DiscoveredProject[]> {
    const data = await vercelFetch<{ projects: VercelProject[] }>('/v9/projects?limit=100', creds)

    return data.projects.map((p) => ({
      id: p.id,
      name: p.name,
      framework: p.framework ?? undefined,
      updatedAt: p.updatedAt ? new Date(p.updatedAt).toISOString() : undefined,
      gitUrl: p.link?.repo
        ? `https://github.com/${p.link.repoOwner ?? p.link.org}/${p.link.repo}`
        : undefined,
      metadata: {
        ...(p.framework ? { framework: p.framework } : {}),
        ...(p.link?.productionBranch ? { branch: p.link.productionBranch } : {}),
      },
    }))
  }

  async snapshotProject(creds: PlatformCredentials, projectId: string): Promise<ProjectSnapshot> {
    // Fetch project details and env vars in parallel
    const [project, envData] = await Promise.all([
      vercelFetch<VercelProject>(`/v9/projects/${projectId}`, creds),
      vercelFetch<{ envs: VercelEnvVar[] }>(
        `/v10/projects/${projectId}/env?decrypt=true`,
        creds
      ).catch(() => ({ envs: [] })),
    ])

    // Map git info
    const git = this.extractGitInfo(project)

    // Map env vars
    const envVars = this.extractEnvVars(envData.envs)

    // Detect services from env vars
    const services = this.detectServices(envVars)

    // Map domains
    const domains = this.extractDomains(project)

    // Map build info
    const build = this.extractBuildInfo(project)

    return {
      id: project.id,
      name: project.name,
      framework: project.framework ?? undefined,
      git,
      envVars,
      services,
      domains,
      build,
      sourceMetadata: project,
    }
  }

  generatePlan(snapshot: ProjectSnapshot): MigrationPlan {
    const preset = inferPreset(snapshot.framework, snapshot.build?.type)

    const project = {
      name: snapshot.name,
      preset,
      directory: '.',
      mainBranch: snapshot.git?.defaultBranch ?? 'main',
      git: snapshot.git,
      buildCommand: snapshot.build?.buildCommand,
      installCommand: snapshot.build?.installCommand,
      outputDir: snapshot.build?.outputDirectory,
    }

    // Build env var plans — skip service-related env vars if we're creating the service
    const serviceEnvKeys = new Set(snapshot.services.flatMap((s) => s.envVarKeys))
    const envVars: EnvVarPlan[] = snapshot.envVars.map((ev) => ({
      key: ev.key,
      value: ev.value,
      isSecret: ev.isSecret,
      source: ev.source,
      skip: serviceEnvKeys.has(ev.key), // Skip env vars that will be auto-set by service creation
    }))

    // Build service plans
    const services: ServicePlan[] = snapshot.services.map((svc) => this.buildServicePlan(svc))

    // Build domain plans
    const domains: DomainPlan[] = snapshot.domains.map((d) => ({
      domain: d.domain,
      action: 'import' as const,
      actionDescription: d.redirectTo
        ? `Import redirect: ${d.domain} → ${d.redirectTo} (${d.redirectStatusCode ?? 301})`
        : `Import domain ${d.domain}`,
      redirectTo: d.redirectTo,
      statusCode: d.redirectStatusCode,
    }))

    // Detect unsupported features
    const unsupportedFeatures = this.detectUnsupportedFeatures(snapshot)

    const steps = buildSteps({ project, envVars, services, domains })
    const summary = buildSummary('vercel', { envVars, services, domains }, unsupportedFeatures)

    return {
      platform: 'vercel',
      sourceProjectId: snapshot.id,
      sourceProjectName: snapshot.name,
      project,
      envVars,
      services,
      domains,
      steps,
      summary,
      unsupportedFeatures,
    }
  }

  // -------------------------------------------------------------------------
  // Private helpers
  // -------------------------------------------------------------------------

  private extractGitInfo(project: VercelProject): GitInfo | undefined {
    const link = project.link
    if (!link) return undefined

    const owner = link.repoOwner ?? link.org
    const repo = link.repo
    if (!owner || !repo) return undefined

    let provider: GitInfo['provider'] = 'unknown'
    const type = (link.type ?? '').toLowerCase()
    if (type.includes('github')) provider = 'github'
    else if (type.includes('gitlab')) provider = 'gitlab'
    else if (type.includes('bitbucket')) provider = 'bitbucket'

    return {
      provider,
      owner,
      repo,
      defaultBranch: link.productionBranch ?? 'main',
      cloneUrl: `https://github.com/${owner}/${repo}.git`,
    }
  }

  private extractEnvVars(envs: VercelEnvVar[]): EnvVar[] {
    return envs
      .filter((e) => {
        // Only include production-targeted env vars (or env vars without target)
        if (!e.target || e.target.length === 0) return true
        return e.target.includes('production')
      })
      .map((e) => ({
        key: e.key,
        value: e.value ?? '',
        source: `vercel:${e.type}${e.target ? ` [${e.target.join(',')}]` : ''}`,
        isSecret: e.type === 'secret' || e.type === 'encrypted' || e.type === 'sensitive' || isLikelySecret(e.key),
        isBuildTime: false,
      }))
  }

  private detectServices(envVars: EnvVar[]): ServiceSnapshot[] {
    const serviceMap = new Map<string, ServiceSnapshot>()

    for (const ev of envVars) {
      // Check key pattern
      let svcType = detectServiceTypeFromKey(ev.key)

      // Check value if it looks like a URL
      if (!svcType && ev.value && (ev.value.includes('://') || ev.value.includes('localhost'))) {
        svcType = detectServiceTypeFromUrl(ev.value)
      }

      if (svcType) {
        const existing = serviceMap.get(svcType)
        if (existing) {
          existing.envVarKeys.push(ev.key)
        } else {
          serviceMap.set(svcType, {
            id: `detected-${svcType}`,
            name: svcType,
            type: svcType,
            hasData: true, // Assume existing services have data
            envVarKeys: [ev.key],
            connectionUrl: ev.value.includes('://') ? ev.value : undefined,
          })
        }
      }
    }

    return Array.from(serviceMap.values())
  }

  private extractDomains(project: VercelProject): DomainSnapshot[] {
    if (!project.alias || !Array.isArray(project.alias)) return []

    return project.alias
      .filter((a) => a.domain && !a.domain.endsWith('.vercel.app'))
      .map((a) => ({
        domain: a.domain,
        isApex: !a.domain.includes('.') || a.domain.split('.').length === 2,
        redirectTo: a.redirect ?? undefined,
        redirectStatusCode: a.redirectStatusCode,
      }))
  }

  private extractBuildInfo(project: VercelProject): BuildInfo | undefined {
    if (!project.buildCommand && !project.installCommand && !project.outputDirectory && !project.framework) {
      return undefined
    }

    return {
      type: project.framework ?? 'unknown',
      buildCommand: project.buildCommand ?? undefined,
      installCommand: project.installCommand ?? undefined,
      outputDirectory: project.outputDirectory ?? undefined,
    }
  }

  private buildServicePlan(svc: ServiceSnapshot): ServicePlan {
    const dataImplications = []

    if (svc.hasData) {
      dataImplications.push({
        severity: 'data-not-migrated' as const,
        message: `${svc.type} data is NOT migrated. Only the service container is created.`,
        recommendedAction: `Export data from the source ${svc.type} and import it into the new Temps service after migration.`,
      })
    }

    if (svc.connectionUrl) {
      dataImplications.push({
        severity: 'warning' as const,
        message: `The source connection URL references an external ${svc.type} instance. Temps will create a NEW local instance.`,
        recommendedAction: 'Update your application to use the new connection URL provided by Temps, or choose "link-external" to keep using the existing service.',
      })
    }

    return {
      name: svc.name,
      type: svc.type,
      version: svc.version,
      action: 'create',
      actionDescription: `Create a new ${svc.type} service in Temps (data NOT migrated)`,
      envVarKeys: svc.envVarKeys,
      dataImplications,
    }
  }

  private detectUnsupportedFeatures(snapshot: ProjectSnapshot): UnsupportedFeature[] {
    const unsupported: UnsupportedFeature[] = []
    const meta = snapshot.sourceMetadata as VercelProject | undefined

    // Vercel serverless functions → Temps runs containers
    if (meta?.framework === null && !meta?.buildCommand) {
      unsupported.push({
        feature: 'Serverless Functions (API Routes without framework)',
        reason: 'Temps runs containers, not serverless functions. Your app needs a build command.',
        alternative: 'Add a build command and entry point, or use a framework like Next.js.',
      })
    }

    // Edge functions
    unsupported.push({
      feature: 'Vercel Edge Functions / Edge Middleware',
      reason: 'Edge runtime is Vercel-specific and not supported in container deployments.',
      alternative: 'Move edge logic to standard server-side middleware in your application.',
    })

    // Vercel Analytics / Speed Insights
    unsupported.push({
      feature: 'Vercel Analytics / Speed Insights',
      reason: 'Vercel Analytics is a proprietary service.',
      alternative: 'Use Temps built-in analytics or integrate an alternative like Plausible or PostHog.',
    })

    // Vercel Blob / KV / Postgres (managed services)
    const vercelServiceKeys = ['BLOB_READ_WRITE_TOKEN', 'KV_REST_API_URL', 'POSTGRES_URL', 'EDGE_CONFIG']
    const foundVercelServices = snapshot.envVars.filter((ev) => vercelServiceKeys.includes(ev.key))
    if (foundVercelServices.length > 0) {
      unsupported.push({
        feature: `Vercel managed services (${foundVercelServices.map((e) => e.key).join(', ')})`,
        reason: 'Vercel Blob/KV/Postgres/Edge Config are proprietary managed services.',
        alternative: 'Use Temps external services (PostgreSQL, Redis, S3) as replacements.',
      })
    }

    // Image Optimization
    unsupported.push({
      feature: 'Vercel Image Optimization',
      reason: 'Vercel Image Optimization is a proprietary CDN feature.',
      alternative: 'Use next/image with a custom loader, or an external image CDN.',
    })

    return unsupported
  }
}
