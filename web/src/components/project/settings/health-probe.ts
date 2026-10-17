// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { HealthCheckConfig } from '@/api/client'
import { z } from 'zod'

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

/** Hidden controls retain their values when the selected probe changes. */
export const healthProbeFormSchema = z
  .object({
    probeType: z.enum(['auto', 'http', 'tcp', 'process', 'exec']),
    stableFor: z.number().or(z.nan()),
    probeTimeout: z.number().or(z.nan()),
    probeInterval: z.number().or(z.nan()),
    probeCommand: z.string(),
    healthPath: z.string(),
  })
  .superRefine((values, ctx) => {
    const range = (
      field: 'stableFor' | 'probeTimeout' | 'probeInterval',
      max: number
    ) => {
      if (
        !Number.isInteger(values[field]) ||
        values[field] < 1 ||
        values[field] > max
      ) {
        ctx.addIssue({
          code: 'custom',
          path: [field],
          message: `Enter a whole number between 1 and ${max}`,
        })
      }
    }
    if (values.probeType === 'process' || values.probeType === 'auto')
      range('stableFor', 3600)
    if (values.probeType === 'tcp' || values.probeType === 'exec') {
      range('probeTimeout', 300)
      range('probeInterval', 3600)
    }
    if (values.probeType === 'http' && !values.healthPath.startsWith('/')) {
      ctx.addIssue({
        code: 'custom',
        path: ['healthPath'],
        message: 'Path must start with /',
      })
    }
    if (
      values.probeType === 'exec' &&
      !parseHealthCommand(values.probeCommand)
    ) {
      ctx.addIssue({
        code: 'custom',
        path: ['probeCommand'],
        message:
          'Enter a JSON argument array, for example ["redis-cli", "ping"]',
      })
    }
  })

export function healthConfigFromForm(
  values: z.infer<typeof healthProbeFormSchema>
): HealthCheckConfig {
  const timed = values.probeType === 'tcp' || values.probeType === 'exec'
  return {
    type: values.probeType === 'auto' ? undefined : values.probeType,
    stable_for:
      values.probeType === 'process' || values.probeType === 'auto'
        ? values.stableFor
        : 15,
    command:
      values.probeType === 'exec'
        ? (parseHealthCommand(values.probeCommand) ?? [])
        : [],
    timeout: timed ? values.probeTimeout : 5,
    interval: timed ? values.probeInterval : 30,
    path:
      values.probeType === 'http' ||
      (values.probeType === 'auto' && values.healthPath.startsWith('/'))
        ? values.healthPath
        : '/',
    status: 200,
    retries: 3,
  }
}
