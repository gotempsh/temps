/**
 * Core types for the client-side migration system.
 *
 * These types define the normalized migration plan that is:
 * 1. Generated from any source platform (Vercel, Coolify, Dokploy, etc.)
 * 2. Presented to the user for review with full transparency
 * 3. Executed against the Temps server API
 *
 * Design principles:
 * - Safety over speed: every action is explicit, every risk is surfaced
 * - Users can skip/modify individual items before execution
 * - Verification steps run before AND after migration
 */

// ---------------------------------------------------------------------------
// Source platform identity
// ---------------------------------------------------------------------------

// Coolify/Dokploy used to have client-side adapters here too, but temps has
// a real, deploy-and-verified backend importer for them now (`temps-import`
// + `temps migrate` in this same CLI, resolveCredentials/pickSource in
// `../imports/`) — this client-side path is Vercel-only until a backend
// importer exists for it too.
export type PlatformId = 'vercel'

export interface PlatformInfo {
  id: PlatformId
  name: string
  description: string
  requiresToken: boolean
  requiresBaseUrl: boolean
  docsUrl: string
  /** Step-by-step instructions for generating the API token */
  tokenInstructions: string[]
}

export const PLATFORMS: Record<PlatformId, PlatformInfo> = {
  vercel: {
    id: 'vercel',
    name: 'Vercel',
    description: 'Migrate projects from Vercel (serverless, static, Next.js)',
    requiresToken: true,
    requiresBaseUrl: false,
    docsUrl: 'https://vercel.com/docs/rest-api',
    tokenInstructions: [
      'Go to https://vercel.com/account/tokens',
      'Click "Create Token"',
      'Set scope to "Full Account" (or select your team)',
      'Set expiration to at least 1 hour',
      'Copy the generated token',
    ],
  },
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

export interface PlatformCredentials {
  token: string
  teamId?: string
  baseUrl?: string
}

// ---------------------------------------------------------------------------
// Discovered project (what the source platform has)
// ---------------------------------------------------------------------------

export interface DiscoveredProject {
  /** Unique ID on the source platform */
  id: string
  /** Display name */
  name: string
  /** Framework or type (e.g., "Next.js", "Docker", "nixpacks") */
  framework?: string
  /** Last updated */
  updatedAt?: string
  /** Git repo URL if known */
  gitUrl?: string
  /** Additional metadata for display */
  metadata?: Record<string, string>
}

// ---------------------------------------------------------------------------
// Project snapshot (full detail from source platform)
// ---------------------------------------------------------------------------

export interface ProjectSnapshot {
  id: string
  name: string
  framework?: string

  /** Git source info */
  git?: GitInfo

  /** Environment variables */
  envVars: EnvVar[]

  /** Attached services (databases, caches, etc.) */
  services: ServiceSnapshot[]

  /** Custom domains */
  domains: DomainSnapshot[]

  /** Build configuration */
  build?: BuildInfo

  /** Raw metadata from source (for debugging) */
  sourceMetadata?: unknown
}

export interface GitInfo {
  provider: 'github' | 'gitlab' | 'bitbucket' | 'gitea' | 'unknown'
  owner: string
  repo: string
  defaultBranch: string
  cloneUrl?: string
}

export interface EnvVar {
  key: string
  value: string
  /** Where this came from for traceability */
  source?: string
  /** Whether this looks like a secret */
  isSecret: boolean
  /** Whether this is a build-time only variable */
  isBuildTime?: boolean
}

export interface ServiceSnapshot {
  id: string
  name: string
  type: 'postgres' | 'mysql' | 'redis' | 'mongodb' | 's3' | 'unknown'
  version?: string
  /** Connection URL if available */
  connectionUrl?: string
  /** Whether the service has existing data */
  hasData: boolean
  /** Env var keys that reference this service */
  envVarKeys: string[]
}

export interface DomainSnapshot {
  domain: string
  isApex: boolean
  /** Whether it's a redirect */
  redirectTo?: string
  redirectStatusCode?: number
}

export interface BuildInfo {
  /** nixpacks, dockerfile, static, buildpack */
  type: string
  buildCommand?: string
  installCommand?: string
  outputDirectory?: string
  dockerfilePath?: string
}

// ---------------------------------------------------------------------------
// Migration plan (what we're going to do)
// ---------------------------------------------------------------------------

export interface MigrationPlan {
  /** Source platform */
  platform: PlatformId
  /** Source project ID */
  sourceProjectId: string
  /** Source project name */
  sourceProjectName: string

  /** Project to create in Temps */
  project: ProjectPlan

  /** Environment variables to set */
  envVars: EnvVarPlan[]

  /** Services to handle */
  services: ServicePlan[]

  /** Domains to handle */
  domains: DomainPlan[]

  /** Ordered execution steps (derived from above) */
  steps: MigrationStep[]

  /** Summary for the user */
  summary: MigrationSummary

  /** Features that cannot be migrated */
  unsupportedFeatures: UnsupportedFeature[]
}

export interface ProjectPlan {
  name: string
  preset: string
  directory: string
  mainBranch: string
  git?: GitInfo
  buildCommand?: string
  installCommand?: string
  outputDir?: string
  exposedPort?: number
}

export interface EnvVarPlan {
  key: string
  value: string
  isSecret: boolean
  source?: string
  /** User can skip individual env vars */
  skip: boolean
}

export interface ServicePlan {
  name: string
  type: 'postgres' | 'mysql' | 'redis' | 'mongodb' | 's3' | 'unknown'
  version?: string
  /** What to do with this service */
  action: 'create' | 'link-external' | 'skip'
  /** Human-readable explanation */
  actionDescription: string
  /** Env var mappings from source to Temps */
  envVarKeys: string[]
  /** Data implications */
  dataImplications: DataImplication[]
}

export interface DomainPlan {
  domain: string
  /** What to do with this domain */
  action: 'import' | 'skip'
  /** Human-readable explanation */
  actionDescription: string
  redirectTo?: string
  statusCode?: number
}

// ---------------------------------------------------------------------------
// Migration steps (execution contract)
// ---------------------------------------------------------------------------

export interface MigrationStep {
  order: number
  id: string
  title: string
  description: string
  resourceType: 'project' | 'environment' | 'env-var' | 'service' | 'domain' | 'git' | 'deployment'
  risk: RiskLevel
  dataImplications: DataImplication[]
  skippable: boolean
  skipped: boolean
  reversible: boolean
  estimatedDuration?: string
}

export type RiskLevel = 'none' | 'low' | 'medium' | 'high' | 'critical'

export interface DataImplication {
  severity: 'info' | 'warning' | 'data-not-migrated' | 'potential-data-loss'
  message: string
  recommendedAction?: string
}

// ---------------------------------------------------------------------------
// Migration summary
// ---------------------------------------------------------------------------

export interface MigrationSummary {
  headline: string
  overallRisk: RiskLevel
  counts: {
    envVars: number
    services: number
    domains: number
  }
  criticalWarnings: string[]
  manualActions: ManualAction[]
}

export interface ManualAction {
  timing: 'before' | 'after' | 'within-hours'
  description: string
  reason: string
}

export interface UnsupportedFeature {
  feature: string
  reason: string
  alternative?: string
}

// ---------------------------------------------------------------------------
// Execution results
// ---------------------------------------------------------------------------

export interface MigrationResult {
  success: boolean
  projectId?: number
  projectSlug?: string
  environmentId?: number
  /** Per-step results */
  stepResults: StepResult[]
  /** Total duration in ms */
  durationMs: number
}

export interface StepResult {
  stepId: string
  title: string
  success: boolean
  skipped: boolean
  message: string
  durationMs: number
  /** Created resource (for audit/rollback) */
  createdResource?: {
    type: string
    id: number
    name: string
  }
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

export interface VerificationCheck {
  id: string
  name: string
  description: string
  /** 'pre' = before migration, 'post' = after migration */
  phase: 'pre' | 'post'
}

export interface VerificationResult {
  checkId: string
  name: string
  passed: boolean
  message: string
  /** 'error' = blocks migration, 'warning' = user can proceed */
  severity: 'error' | 'warning' | 'info'
}
