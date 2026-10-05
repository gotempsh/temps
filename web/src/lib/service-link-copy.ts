// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Service-type-specific copy for what linking a project to a managed service
 * actually gives it. Linking itself only records the link; each engine creates
 * the per-environment resource on that environment's first deployment (see
 * each provider's `get_runtime_env_vars`), and they differ:
 *
 * - PostgreSQL / MariaDB / MongoDB: a named database (`<project>_<env>` by
 *   default; per-project or custom names are also possible).
 * - Redis / KV: a logical database index (1-15) on the shared instance.
 * - S3 / RustFS / MinIO / Blob: a bucket named `<project>-<env>`.
 */

export type LinkedResourceKind = 'database' | 'redis' | 'bucket' | 'other'

export function linkedResourceKind(serviceType: string): LinkedResourceKind {
  switch (serviceType) {
    case 'postgres':
    case 'mariadb':
    case 'mongodb':
      return 'database'
    case 'redis':
    case 'kv':
      return 'redis'
    case 's3':
    case 'rustfs':
    case 'minio':
    case 'blob':
      return 'bucket'
    default:
      return 'other'
  }
}

export interface LinkedResourceCopy {
  /** Sentence before the example name, e.g. "Each linked environment gets its own". */
  lead: string
  /** Example resource name rendered as code, when the engine has one. */
  example?: string
  /** Sentence after the example name. */
  trail: string
}

/** What a linked project gets from this service, per environment. */
export function linkedResourceCopy(serviceType: string): LinkedResourceCopy {
  switch (linkedResourceKind(serviceType)) {
    case 'database':
      return {
        lead: 'With the default per-environment strategy, each environment gets a database named',
        example: '<project>_<env>',
        trail:
          'on its first deployment. Per-project and custom database strategies can share a database. No extra services are spun up.',
      }
    case 'redis':
      return {
        lead: 'Each linked environment gets its own Redis logical database (index 1-15) on this instance, assigned on its first deployment. One instance holds up to 15 project environments.',
        trail: 'No extra services are spun up.',
      }
    case 'bucket':
      return {
        lead: 'Each linked environment gets its own bucket, named',
        example: '<project>-<env>',
        trail:
          'created on its first deployment. No extra services are spun up.',
      }
    case 'other':
      return {
        lead: "Linked projects receive this service's connection variables on their next deployment.",
        trail: 'No extra services are spun up.',
      }
  }
}

/** Hint next to the "Advanced configuration" toggle on the create form. */
export function advancedParamsHint(serviceType: string): string {
  switch (linkedResourceKind(serviceType)) {
    case 'database':
      return '(default database and admin credentials. Links use a per-environment database strategy by default)'
    case 'redis':
      return '(instance password. Each linked project environment gets its own logical database automatically)'
    case 'bucket':
      return '(root access keys. Each linked project environment gets its own bucket automatically)'
    case 'other':
      return '(credentials and connection defaults)'
  }
}

const PER_ENVIRONMENT_RESOURCE: Record<LinkedResourceKind, string | null> = {
  database:
    'databases according to its configured naming strategy (per environment by default)',
  redis: "a Redis logical database for each of this project's environments",
  bucket: "a bucket for each of this project's environments",
  other: null,
}

/** Body of the "Create and link to <project>" notice. */
export function createAndLinkDescription(serviceType: string): string {
  const resource = PER_ENVIRONMENT_RESOURCE[linkedResourceKind(serviceType)]
  const provisioning = resource
    ? `On its next deployment, the project gets ${resource} and the matching runtime variables.`
    : 'On its next deployment, the project receives the runtime variables for this service.'
  return `Temps will create the service, link it to this project, and update its application sandbox network as one operation. ${provisioning}`
}
