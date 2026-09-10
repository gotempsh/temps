// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ManagedEnvironmentVariable,
  ManagedEnvironmentVariableSource,
} from '@/api/client/types.gen'

export interface ProvidedEnvironmentVariableCollision {
  name: string
  provider: string
  isUserOverridable: boolean
}

const SOURCE_ORDER: ManagedEnvironmentVariableSource[] = [
  'error_tracking',
  'open_telemetry',
  'temps',
]

/// Catalog-only slugs produced by `PresetInfo::catalog_slug()`
/// (`crates/temps-presets`) that group several frameworks under one buildable
/// entry. They aren't in the backend's `Preset::from_str` vocabulary, so they
/// must be mapped to the closest preset it does recognize before being sent
/// to `/deployments/managed-environment-variables`.
const CATALOG_ONLY_PRESET_ALIASES: Record<string, string> = {
  'nixpacks-node': 'nodejs',
  'nixpacks-static': 'static',
}

export function normalizeCreationPreset(preset: string): string {
  const [name] = preset.split('::')
  const normalized = name.trim().toLowerCase() || 'dockerfile'
  if (normalized in CATALOG_ONLY_PRESET_ALIASES) {
    return CATALOG_ONLY_PRESET_ALIASES[normalized]
  }
  return normalized === 'custom' ? 'static' : normalized
}

export function groupManagedEnvironmentVariables(
  variables: ManagedEnvironmentVariable[]
) {
  return SOURCE_ORDER.map((source) => ({
    source,
    variables: variables.filter((variable) => variable.source === source),
  })).filter((group) => group.variables.length > 0)
}

export function findProvidedEnvironmentVariableCollision(
  variableName: string,
  providedVariables: ProvidedEnvironmentVariableCollision[]
) {
  const normalizedName = variableName.trim()
  return providedVariables.find((variable) => variable.name === normalizedName)
}

export function isNonOverridableProvidedEnvironmentVariable(
  variableName: string,
  providedVariables: ProvidedEnvironmentVariableCollision[]
): boolean {
  return (
    findProvidedEnvironmentVariableCollision(variableName, providedVariables)
      ?.isUserOverridable === false
  )
}

export function databaseProvidedEnvironmentVariable(
  name: string,
  databaseName: string
): ProvidedEnvironmentVariableCollision {
  return {
    name,
    provider: `database "${databaseName}"`,
    isUserOverridable: true,
  }
}
