// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod/v4'

type TemplateRuntimeSource = {
  image?: string | null
  command?: string[] | null
  resources?: {
    cpu_request?: number | null
    cpu_limit?: number | null
    memory_request?: number | null
    memory_limit?: number | null
  } | null
  exposed_port?: number | null
  health_check_path?: string | null
}

type ServiceTemplateProjectSource = {
  preset_config?: unknown
  deployment_config?: {
    cpuRequest?: number | null
    cpuLimit?: number | null
    memoryRequest?: number | null
    memoryLimit?: number | null
    exposedPort?: number | null
  } | null
}

type StoredImageRuntime = {
  exists: boolean
  imageRef?: unknown
  command?: unknown
  healthCheckPath?: unknown
}

const optionalNumber = (
  label: string,
  options: { min: number; allowZero?: boolean }
) =>
  z.string().refine(
    (value) => {
      if (value.trim() === '') return true
      const parsed = Number(value)
      if (!Number.isFinite(parsed)) return false
      if (options.allowZero && parsed === 0) return true
      return parsed >= options.min
    },
    `${label} must be ${options.allowZero ? '0 or ' : ''}at least ${options.min}`
  )

function hasControlCharacters(value: string): boolean {
  return Array.from(value).some((character) => {
    const codePoint = character.codePointAt(0)
    return codePoint != null && (codePoint <= 31 || codePoint === 127)
  })
}

function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).length
}

export const templateRuntimeDefaultsSchema = z
  .object({
    image: z
      .string()
      .trim()
      .min(1, 'Image reference is required')
      .refine(
        (value) => utf8ByteLength(value) <= 512,
        'Image reference cannot exceed 512 bytes'
      )
      .regex(
        /^.+@sha256:[0-9a-f]{64}$/i,
        'Image reference must use an immutable SHA-256 digest'
      )
      .refine(
        (value) =>
          !Array.from(value).some((character) => /\s/.test(character)) &&
          !hasControlCharacters(value),
        'Image reference cannot contain whitespace or control characters'
      ),
    command: z.string().superRefine((value, context) => {
      const argumentsList = value
        .split('\n')
        .map((argument) => argument.trim())
        .filter(Boolean)
      if (argumentsList.length > 64) {
        context.addIssue({
          code: 'custom',
          message: 'Container command supports at most 64 arguments',
        })
      }
      if (
        argumentsList.some(
          (argument) =>
            utf8ByteLength(argument) > 1_024 || hasControlCharacters(argument)
        )
      ) {
        context.addIssue({
          code: 'custom',
          message:
            'Each command argument must be at most 1024 bytes and contain no control characters',
        })
      }
    }),
    cpuRequest: optionalNumber('CPU request', { min: 0.01 }),
    cpuLimit: optionalNumber('CPU limit', { min: 0.01, allowZero: true }),
    memoryRequest: optionalNumber('Memory request', { min: 1 }),
    memoryLimit: optionalNumber('Memory limit', { min: 1, allowZero: true }),
    exposedPort: z
      .string()
      .refine(
        (value) =>
          value.trim() === '' ||
          (Number.isInteger(Number(value)) &&
            Number(value) >= 1 &&
            Number(value) <= 65_535),
        'Port must be between 1 and 65535'
      ),
    healthCheckPath: z
      .string()
      .trim()
      .min(1, 'Health-check path is required')
      .refine(
        (value) => utf8ByteLength(value) <= 2_048,
        'Health-check path cannot exceed 2048 bytes'
      )
      .refine(
        (value) =>
          value.startsWith('/') &&
          !value.includes('://') &&
          !value.includes('@') &&
          !hasControlCharacters(value),
        "Use a relative HTTP path starting with '/'"
      ),
  })
  .superRefine((runtime, context) => {
    const cpuRequest = optionalNumericValue(runtime.cpuRequest)
    const cpuLimit = optionalNumericValue(runtime.cpuLimit)
    if (
      cpuRequest != null &&
      cpuLimit != null &&
      cpuLimit !== 0 &&
      cpuRequest > cpuLimit
    ) {
      context.addIssue({
        code: 'custom',
        path: ['cpuLimit'],
        message: 'CPU limit must be greater than or equal to the request',
      })
    }

    const memoryRequest = optionalNumericValue(runtime.memoryRequest)
    const memoryLimit = optionalNumericValue(runtime.memoryLimit)
    if (
      memoryRequest != null &&
      memoryLimit != null &&
      memoryLimit !== 0 &&
      memoryRequest > memoryLimit
    ) {
      context.addIssue({
        code: 'custom',
        path: ['memoryLimit'],
        message: 'Memory limit must be greater than or equal to the request',
      })
    }
  })

export type TemplateRuntimeDefaults = z.infer<
  typeof templateRuntimeDefaultsSchema
>

export type TemplateRuntimeOverrides = {
  image: string
  command: string[]
  cpu_request?: number
  cpu_limit?: number
  memory_request?: number
  memory_limit?: number
  exposed_port?: number
  health_check_path: string
}

function optionalNumericValue(value: string): number | undefined {
  const normalized = value.trim()
  return normalized === '' ? undefined : Number(normalized)
}

function formatCores(microcores: number | null | undefined): string {
  return microcores == null ? '' : String(microcores / 1_000_000)
}

function formatInteger(value: number | null | undefined): string {
  return value == null ? '' : String(value)
}

export function templateRuntimeDefaults(
  template: TemplateRuntimeSource
): TemplateRuntimeDefaults {
  return {
    image: template.image ?? '',
    command: template.command?.join('\n') ?? '',
    cpuRequest: formatCores(template.resources?.cpu_request),
    cpuLimit: formatCores(template.resources?.cpu_limit),
    memoryRequest: formatInteger(template.resources?.memory_request),
    memoryLimit: formatInteger(template.resources?.memory_limit),
    exposedPort: formatInteger(template.exposed_port),
    healthCheckPath: template.health_check_path ?? '/',
  }
}

function storedImageRuntime(
  project: ServiceTemplateProjectSource
): StoredImageRuntime {
  if (!project.preset_config || typeof project.preset_config !== 'object') {
    return { exists: false }
  }
  const value = (project.preset_config as Record<string, unknown>).imageRuntime
  return value && typeof value === 'object'
    ? { ...(value as Omit<StoredImageRuntime, 'exists'>), exists: true }
    : { exists: false }
}

/**
 * Resolve the durable project settings first and use the current catalog only
 * as a migration fallback for projects created before image runtime settings
 * were persisted.
 */
export function serviceTemplateRuntimeDefaults(
  project: ServiceTemplateProjectSource,
  template: TemplateRuntimeSource
): TemplateRuntimeDefaults {
  const defaults = templateRuntimeDefaults(template)
  const stored = storedImageRuntime(project)
  const deployment = project.deployment_config

  return {
    image:
      typeof stored.imageRef === 'string' ? stored.imageRef : defaults.image,
    command: stored.exists
      ? Array.isArray(stored.command)
        ? stored.command
            .filter((value): value is string => typeof value === 'string')
            .join('\n')
        : ''
      : defaults.command,
    healthCheckPath:
      typeof stored.healthCheckPath === 'string'
        ? stored.healthCheckPath
        : defaults.healthCheckPath,
    cpuRequest: formatCores(deployment?.cpuRequest),
    cpuLimit: formatCores(deployment?.cpuLimit),
    memoryRequest: formatInteger(deployment?.memoryRequest),
    memoryLimit: formatInteger(deployment?.memoryLimit),
    exposedPort: formatInteger(deployment?.exposedPort),
  }
}

export function serviceTemplateDeployOverrides(
  project: ServiceTemplateProjectSource
): {
  image_ref?: string
  command?: string[]
  health_check_path?: string
} {
  const stored = storedImageRuntime(project)
  return {
    ...(typeof stored.imageRef === 'string' && stored.imageRef.trim()
      ? { image_ref: stored.imageRef.trim() }
      : {}),
    ...(Array.isArray(stored.command) &&
    stored.command.length > 0 &&
    stored.command.every((part) => typeof part === 'string')
      ? { command: stored.command as string[] }
      : {}),
    ...(typeof stored.healthCheckPath === 'string' && stored.healthCheckPath
      ? { health_check_path: stored.healthCheckPath }
      : {}),
  }
}

/**
 * Recreate a historical image deployment exactly. A missing/null command is
 * serialized as an empty argv override so the server uses the image default
 * instead of inheriting the project's current saved template command.
 */
export function historicalImageRuntime(
  metadata?: {
    command?: string[] | null
    healthCheckPath?: string | null
  } | null
): {
  command: string[]
  health_check_path: string
} {
  return {
    command: Array.isArray(metadata?.command) ? metadata.command : [],
    health_check_path: metadata?.healthCheckPath ?? '/',
  }
}

export function templateRuntimeOverrides(
  runtime: TemplateRuntimeDefaults
): TemplateRuntimeOverrides {
  const cpuRequest = optionalNumericValue(runtime.cpuRequest)
  const cpuLimit = optionalNumericValue(runtime.cpuLimit)

  return {
    image: runtime.image.trim(),
    command: runtime.command
      .split('\n')
      .map((argument) => argument.trim())
      .filter(Boolean),
    cpu_request:
      cpuRequest == null ? undefined : Math.round(cpuRequest * 1_000_000),
    cpu_limit: cpuLimit == null ? undefined : Math.round(cpuLimit * 1_000_000),
    memory_request: optionalNumericValue(runtime.memoryRequest),
    memory_limit: optionalNumericValue(runtime.memoryLimit),
    exposed_port: optionalNumericValue(runtime.exposedPort),
    health_check_path: runtime.healthCheckPath.trim(),
  }
}
