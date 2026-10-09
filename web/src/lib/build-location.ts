// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Where a source deployment's image is built (`deployment_config.buildLocation`). */
export type BuildLocation = 'control_plane' | 'node'

/** Select value for the tri-state environment build location override. */
export type BuildLocationSelect = 'inherit' | BuildLocation

export const BUILD_LOCATION_LABELS: Record<BuildLocation, string> = {
  control_plane: 'Control plane',
  node: 'Worker node',
}

/**
 * Map an environment's nullable `buildLocation` to the select value.
 * Unset means the environment follows the project.
 */
export function buildLocationToSelect(
  value: BuildLocation | null | undefined
): BuildLocationSelect {
  return value ?? 'inherit'
}

/**
 * Map the select value back to the API payload. "inherit" sends `null` so the
 * server clears the override instead of pinning the current project value.
 */
export function buildLocationToPayload(
  value: BuildLocationSelect
): BuildLocation | null {
  return value === 'inherit' ? null : value
}
