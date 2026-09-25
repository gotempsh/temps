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

function presentVariableKeys(
  variables: EnvironmentVariableResponse[],
  environment: EnvironmentResponse
) {
  return new Set(
    variables
      .filter(
        (variable) =>
          variable.environments.some((entry) => entry.id === environment.id) ||
          (environment.is_preview && variable.include_in_preview)
      )
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
