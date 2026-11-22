// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  EnvironmentInfo,
  EnvironmentResponse,
  EnvironmentVariableResponse,
} from '@/api/client/types.gen'

export function orderEnvironments(environments: EnvironmentResponse[]) {
  return [...environments].sort((a, b) => {
    if (a.is_preview !== b.is_preview) return a.is_preview ? 1 : -1
    if (a.name === 'production') return -1
    if (b.name === 'production') return 1
    return a.name.localeCompare(b.name)
  })
}

export function orderVariableEnvironments(
  environments: EnvironmentInfo[],
  previewIds: ReadonlySet<number>
) {
  return [...environments].sort((a, b) => {
    const aPreview = previewIds.has(a.id)
    const bPreview = previewIds.has(b.id)
    if (aPreview !== bPreview) return aPreview ? 1 : -1
    if (a.name === 'production') return -1
    if (b.name === 'production') return 1
    return a.name.localeCompare(b.name)
  })
}

export interface VariableEnvironmentGroups {
  /** Existing non-preview environments, production first. */
  stable: EnvironmentInfo[]
  /** Existing preview environments. */
  previews: EnvironmentInfo[]
  /**
   * Bindings to deleted environments. The server keeps them so a restored
   * environment gets its variables back, but no deployment uses the links.
   */
  deleted: EnvironmentInfo[]
}

/**
 * Split a variable's bindings for display. `liveIds` holds the project's
 * current environments; while that list is loading or failed it is
 * `undefined` and nothing is classified as deleted.
 */
export function groupVariableEnvironments(
  environments: EnvironmentInfo[],
  previewIds: ReadonlySet<number>,
  liveIds: ReadonlySet<number> | undefined
): VariableEnvironmentGroups {
  const groups: VariableEnvironmentGroups = {
    stable: [],
    previews: [],
    deleted: [],
  }
  for (const environment of orderVariableEnvironments(
    environments,
    previewIds
  )) {
    if (liveIds && !liveIds.has(environment.id)) {
      groups.deleted.push(environment)
    } else if (previewIds.has(environment.id)) {
      groups.previews.push(environment)
    } else {
      groups.stable.push(environment)
    }
  }
  return groups
}

/** Add or remove one environment from a selection, keeping its order. */
export function toggleEnvironmentSelection(
  selectedIds: readonly number[],
  id: number
): number[] {
  return selectedIds.includes(id)
    ? selectedIds.filter((selected) => selected !== id)
    : [...selectedIds, id]
}

/**
 * Select or clear a batch (e.g. every environment the picker's filter shows)
 * without touching selected environments outside it.
 */
export function setEnvironmentSelection(
  selectedIds: readonly number[],
  ids: readonly number[],
  selected: boolean
): number[] {
  if (!selected) return selectedIds.filter((id) => !ids.includes(id))
  return [...selectedIds, ...ids.filter((id) => !selectedIds.includes(id))]
}

/** Explicit bindings plus inherited preview variables determine applicability. */
export function variableAppliesToEnvironment(
  variable: Pick<
    EnvironmentVariableResponse,
    'environments' | 'include_in_preview'
  >,
  environment: Pick<EnvironmentResponse, 'id' | 'is_preview'> | undefined
): boolean {
  return (
    !environment ||
    variable.environments.some((entry) => entry.id === environment.id) ||
    Boolean(environment.is_preview && variable.include_in_preview)
  )
}

function presentVariableKeys(
  variables: EnvironmentVariableResponse[],
  environment: EnvironmentResponse
) {
  return new Set(
    variables
      .filter((variable) => variableAppliesToEnvironment(variable, environment))
      .map((variable) => variable.key)
  )
}

export function compareEnvironmentVariableKeys(
  variables: EnvironmentVariableResponse[],
  first: EnvironmentResponse,
  second: EnvironmentResponse
) {
  const firstKeys = presentVariableKeys(variables, first)
  const secondKeys = presentVariableKeys(variables, second)
  return {
    missingInFirst: [...secondKeys].filter((key) => !firstKeys.has(key)).sort(),
    missingInSecond: [...firstKeys]
      .filter((key) => !secondKeys.has(key))
      .sort(),
  }
}
