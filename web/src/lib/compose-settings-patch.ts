// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type ComposeSettingKey =
  | 'composeOverride'
  | 'publicPorts'
  | 'excludedServices'
  | 'relaxedCapabilityServices'
  | 'unsandboxedServices'

/**
 * Build an explicit Docker Compose settings update. Empty arrays and null are
 * meaningful clear operations and must remain present after JSON serialization;
 * `undefined` would be omitted and the backend would preserve the old value.
 */
export function composeSettingPatch(
  config: Record<string, unknown>,
  key: ComposeSettingKey,
  value: unknown
): Record<string, unknown> {
  return {
    ...config,
    preset: 'docker-compose',
    [key]: value,
  }
}
