// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DatabaseProvisioningMode } from '@/api/client'

export type DatabaseProvisioningSelection = {
  database_provisioning_mode: DatabaseProvisioningMode
  custom_database_name?: string
}

const CUSTOM_DATABASE_PATTERN = /^[a-z_][a-z0-9_]{0,62}$/

export function isValidCustomDatabaseName(name: string): boolean {
  return CUSTOM_DATABASE_PATTERN.test(name)
}

export function buildDatabaseProvisioningSelection(
  mode: DatabaseProvisioningMode,
  customName: string
): DatabaseProvisioningSelection | null {
  if (mode === 'custom') {
    if (!isValidCustomDatabaseName(customName)) return null
    return {
      database_provisioning_mode: mode,
      custom_database_name: customName,
    }
  }
  return { database_provisioning_mode: mode }
}

export type PersistedDatabaseProvisioning = {
  database_provisioning_mode: DatabaseProvisioningMode
  custom_database_name?: string | null
}

/**
 * Resolve the logical resource name shown in and opened by the project database
 * list. These rules mirror each provider's `provision_resource` implementation.
 */
export function projectServiceResourcePath(
  serviceType: string,
  projectSlug: string,
  environment = 'production',
  provisioning?: PersistedDatabaseProvisioning
): string {
  if (['s3', 'rustfs', 'minio'].includes(serviceType)) {
    return `${projectSlug}-${environment}`.replace(/_/g, '-').toLowerCase()
  }

  const scopedName =
    provisioning?.database_provisioning_mode === 'custom'
      ? (provisioning.custom_database_name ?? projectSlug)
      : provisioning?.database_provisioning_mode === 'project'
        ? projectSlug
        : `${projectSlug}_${environment}`

  // MongoDB uses the scoped name verbatim. PostgreSQL and MariaDB normalize
  // DNS-safe project/environment slugs to an SQL identifier capped at 63 bytes.
  if (serviceType === 'mongodb') return scopedName

  const normalized = scopedName.toLowerCase().replace(/[^a-z0-9]/g, '_')
  const prefixed = /^\d/.test(normalized) ? `db_${normalized}` : normalized
  return prefixed.slice(0, 63)
}
