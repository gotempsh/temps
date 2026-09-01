// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Base adapter interface for platform migration.
 *
 * Each source platform (Vercel, Coolify, Dokploy) implements this interface.
 * The adapter is responsible for:
 * 1. Validating credentials against the source platform API
 * 2. Discovering projects (lightweight listing)
 * 3. Taking a full snapshot of a selected project
 * 4. Generating a migration plan from the snapshot
 *
 * Adapters ONLY read from the source platform — they never write to Temps.
 * The orchestrator handles all Temps API calls.
 */

import type {
  PlatformId,
  PlatformCredentials,
  DiscoveredProject,
  ProjectSnapshot,
  MigrationPlan,
  MigrationStep,
  MigrationSummary,
  ServicePlan,
  DomainPlan,
  EnvVarPlan,
  ProjectPlan,
  UnsupportedFeature,
  DataImplication,
  RiskLevel,
} from '../types.js'

// ---------------------------------------------------------------------------
// Adapter interface
// ---------------------------------------------------------------------------

export interface PlatformAdapter {
  /** Which platform this adapter handles */
  readonly platformId: PlatformId

  /**
   * Validate that the provided credentials can reach the source platform.
   * Should make a lightweight API call (e.g. "get current user").
   * Returns a human-readable description of the authenticated account.
   */
  validateCredentials(creds: PlatformCredentials): Promise<CredentialValidation>

  /**
   * List all projects/applications on the source platform.
   * This is a lightweight call — no env vars, no services, just names + IDs.
   */
  discoverProjects(creds: PlatformCredentials): Promise<DiscoveredProject[]>

  /**
   * Take a full snapshot of a single project, including env vars, services,
   * domains, build config, and git info.
   */
  snapshotProject(creds: PlatformCredentials, projectId: string): Promise<ProjectSnapshot>

  /**
   * Generate a migration plan from a snapshot.
   * This is a pure function — no API calls, just transforms data.
   */
  generatePlan(snapshot: ProjectSnapshot): MigrationPlan
}

export interface CredentialValidation {
  valid: boolean
  message: string
  /** E.g. team name, user email, org name */
  accountName?: string
}

// ---------------------------------------------------------------------------
// Plan builder helpers (shared across adapters)
// ---------------------------------------------------------------------------

/**
 * Detect whether an env var key looks like a secret.
 */
export function isLikelySecret(key: string): boolean {
  const secretPatterns = [
    /secret/i,
    /password/i,
    /passwd/i,
    /token/i,
    /api[_-]?key/i,
    /private[_-]?key/i,
    /auth/i,
    /credential/i,
    /access[_-]?key/i,
    /signing/i,
    /encryption/i,
    /jwt/i,
    /session/i,
    /cookie/i,
    /salt/i,
    /hash/i,
  ]
  return secretPatterns.some((p) => p.test(key))
}

/**
 * Detect service type from env var key patterns.
 */
export function detectServiceTypeFromKey(key: string): 'postgres' | 'redis' | 'mongodb' | 's3' | null {
  const lower = key.toLowerCase()
  if (lower.includes('postgres') || lower.includes('pg_') || lower === 'database_url') return 'postgres'
  if (lower.includes('redis')) return 'redis'
  if (lower.includes('mongo')) return 'mongodb'
  if (lower.includes('s3') || lower.includes('aws_bucket') || lower.includes('minio')) return 's3'
  return null
}

/**
 * Detect service type from a connection URL.
 */
export function detectServiceTypeFromUrl(url: string): 'postgres' | 'redis' | 'mongodb' | 's3' | null {
  if (url.startsWith('postgres://') || url.startsWith('postgresql://')) return 'postgres'
  if (url.startsWith('redis://') || url.startsWith('rediss://')) return 'redis'
  if (url.startsWith('mongodb://') || url.startsWith('mongodb+srv://')) return 'mongodb'
  if (url.includes('.s3.') || url.includes('minio')) return 's3'
  return null
}

/**
 * Infer a Temps preset from framework/build info.
 */
export function inferPreset(framework?: string, buildType?: string): string {
  if (!framework && !buildType) return 'nixpacks'

  const fw = (framework ?? '').toLowerCase()
  const bt = (buildType ?? '').toLowerCase()

  if (fw.includes('next') || fw === 'nextjs') return 'nextjs'
  if (fw.includes('nuxt')) return 'nixpacks'
  if (fw.includes('remix')) return 'nixpacks'
  if (fw.includes('astro')) return 'static'
  if (fw.includes('gatsby')) return 'static'
  if (fw.includes('hugo')) return 'static'
  if (fw.includes('svelte') || fw.includes('sveltekit')) return 'nixpacks'
  if (fw.includes('vue')) return 'nixpacks'
  if (fw.includes('react') || fw === 'create-react-app' || fw === 'cra') return 'static'
  if (fw.includes('angular')) return 'nixpacks'
  if (fw.includes('express') || fw.includes('fastify') || fw.includes('node') || fw === 'nodejs') return 'nixpacks'
  if (fw.includes('python') || fw.includes('django') || fw.includes('flask') || fw.includes('fastapi')) return 'nixpacks'
  if (fw.includes('ruby') || fw.includes('rails')) return 'nixpacks'
  if (fw.includes('go') || fw.includes('golang')) return 'nixpacks'
  if (fw.includes('rust')) return 'nixpacks'
  if (fw.includes('php') || fw.includes('laravel')) return 'nixpacks'
  if (fw.includes('docker') || bt === 'dockerfile') return 'docker'
  if (fw.includes('static') || bt === 'static') return 'static'

  return 'nixpacks'
}

/**
 * Build the ordered execution steps from a plan's components.
 */
export function buildSteps(plan: {
  project: ProjectPlan
  envVars: EnvVarPlan[]
  services: ServicePlan[]
  domains: DomainPlan[]
}): MigrationStep[] {
  const steps: MigrationStep[] = []
  let order = 1

  // Step 1: Create project
  steps.push({
    order: order++,
    id: 'create-project',
    title: `Create project "${plan.project.name}"`,
    description: `Creates a new Temps project with preset "${plan.project.preset}" and branch "${plan.project.mainBranch}"`,
    resourceType: 'project',
    risk: 'none',
    dataImplications: [],
    skippable: false,
    skipped: false,
    reversible: true,
  })

  // Step 2: Create services (before env vars — services may generate connection env vars)
  for (const svc of plan.services) {
    if (svc.action === 'skip') continue
    steps.push({
      order: order++,
      id: `service-${svc.name}`,
      title: `${svc.action === 'create' ? 'Create' : 'Link'} ${svc.type} service "${svc.name}"`,
      description: svc.actionDescription,
      resourceType: 'service',
      risk: svc.action === 'create' ? 'low' : 'none',
      dataImplications: svc.dataImplications,
      skippable: true,
      skipped: false,
      reversible: true,
    })
  }

  // Step 3: Set environment variables
  const activeEnvVars = plan.envVars.filter((ev) => !ev.skip)
  if (activeEnvVars.length > 0) {
    steps.push({
      order: order++,
      id: 'set-env-vars',
      title: `Set ${activeEnvVars.length} environment variable(s)`,
      description: `Sets environment variables on the Temps project. ${activeEnvVars.filter((e) => e.isSecret).length} are secrets.`,
      resourceType: 'env-var',
      risk: 'low',
      dataImplications: activeEnvVars.some((e) => e.isSecret)
        ? [
            {
              severity: 'warning',
              message: 'Secrets will be stored in Temps encrypted storage',
              recommendedAction: 'Rotate secrets after migration if they were visible in the source platform',
            },
          ]
        : [],
      skippable: true,
      skipped: false,
      reversible: true,
    })
  }

  // Step 4: Configure git (if git info is available)
  if (plan.project.git) {
    steps.push({
      order: order++,
      id: 'configure-git',
      title: `Configure git: ${plan.project.git.owner}/${plan.project.git.repo}`,
      description: `Links the project to ${plan.project.git.provider} repository`,
      resourceType: 'git',
      risk: 'none',
      dataImplications: [],
      skippable: true,
      skipped: false,
      reversible: true,
    })
  }

  // Step 5: Add custom domains
  for (const domain of plan.domains) {
    if (domain.action === 'skip') continue
    steps.push({
      order: order++,
      id: `domain-${domain.domain}`,
      title: `Add domain "${domain.domain}"`,
      description: domain.actionDescription,
      resourceType: 'domain',
      risk: 'medium',
      dataImplications: [
        {
          severity: 'warning',
          message: 'DNS records must be updated to point to your Temps instance',
          recommendedAction: 'Update DNS after migration and before removing the old platform',
        },
      ],
      skippable: true,
      skipped: false,
      reversible: true,
    })
  }

  return steps
}

/**
 * Build a summary from plan components.
 */
export function buildSummary(
  platformId: PlatformId,
  plan: {
    envVars: EnvVarPlan[]
    services: ServicePlan[]
    domains: DomainPlan[]
  },
  unsupportedFeatures: UnsupportedFeature[]
): MigrationSummary {
  const activeServices = plan.services.filter((s) => s.action !== 'skip')
  const activeDomains = plan.domains.filter((d) => d.action !== 'skip')
  const activeEnvVars = plan.envVars.filter((e) => !e.skip)

  // Determine overall risk
  let overallRisk: RiskLevel = 'none'
  const hasDataServices = plan.services.some(
    (s) => s.dataImplications.some((d) => d.severity === 'potential-data-loss' || d.severity === 'data-not-migrated')
  )
  const hasDomains = activeDomains.length > 0

  if (hasDataServices) overallRisk = 'high'
  else if (hasDomains) overallRisk = 'medium'
  else if (activeServices.length > 0) overallRisk = 'low'

  const criticalWarnings: string[] = []
  if (hasDataServices) {
    criticalWarnings.push(
      'Database data is NOT automatically migrated. You must manually export/import data.'
    )
  }
  if (hasDomains) {
    criticalWarnings.push(
      'DNS changes are required. Update DNS records AFTER migration, BEFORE removing the old platform.'
    )
  }
  if (unsupportedFeatures.length > 0) {
    criticalWarnings.push(
      `${unsupportedFeatures.length} feature(s) cannot be migrated automatically.`
    )
  }

  const manualActions = []
  if (hasDataServices) {
    manualActions.push({
      timing: 'before' as const,
      description: 'Export database data from the source platform',
      reason: 'Database data is not migrated — only the service configuration is created',
    })
    manualActions.push({
      timing: 'after' as const,
      description: 'Import database data into the new Temps services',
      reason: 'Data must be restored manually after services are provisioned',
    })
  }
  if (hasDomains) {
    manualActions.push({
      timing: 'after' as const,
      description: 'Update DNS records to point to your Temps instance',
      reason: 'Domains will not resolve until DNS is updated',
    })
  }

  return {
    headline: `Migrate ${activeEnvVars.length} env vars, ${activeServices.length} service(s), and ${activeDomains.length} domain(s) from ${platformId}`,
    overallRisk,
    counts: {
      envVars: activeEnvVars.length,
      services: activeServices.length,
      domains: activeDomains.length,
    },
    criticalWarnings,
    manualActions,
  }
}
