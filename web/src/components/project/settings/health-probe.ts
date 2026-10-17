// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { HealthCheckConfig } from '@/api/client'

/** Parse an exec argument vector without interpreting shell syntax. */
export function parseHealthCommand(value: string): string[] | null {
  try {
    const command: unknown = JSON.parse(value)
    if (
      !Array.isArray(command) ||
      command.length === 0 ||
      command.some((arg) => typeof arg !== 'string') ||
      !command[0].trim()
    )
      return null
    return command
  } catch {
    return null
  }
}

export type ProbeType = 'auto' | 'http' | 'tcp' | 'process' | 'exec'

export function configuredProbeType(
  health: HealthCheckConfig | null | undefined
): ProbeType {
  switch (health?.type) {
    case 'http':
    case 'tcp':
    case 'process':
    case 'exec':
      return health.type
    default:
      return 'auto'
  }
}
