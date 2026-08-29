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

export function normalizeCreationPreset(preset: string): string {
  const [name] = preset.split('::')
  return name.trim().toLowerCase() || 'dockerfile'
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
