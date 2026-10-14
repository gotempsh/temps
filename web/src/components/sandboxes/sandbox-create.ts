// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type { CreateSandboxBody } from '@/api/client'

/** The API clamps the idle timeout to this range (seconds). */
export const SANDBOX_TIMEOUT_MIN_SECS = 60
export const SANDBOX_TIMEOUT_MAX_SECS = 86_400
/** Matches the backend's preview-password length bounds. */
export const PREVIEW_PASSWORD_MIN_LEN = 8
export const PREVIEW_PASSWORD_MAX_LEN = 256
/** `auto` lets Temps place the sandbox. */
export const AUTO_NODE = 'auto'

const optionalNumber = (label: string, min: number, max: number) =>
  z
    .string()
    .trim()
    .refine(
      (value) =>
        value === '' ||
        (/^\d+(\.\d+)?$/.test(value) &&
          Number(value) >= min &&
          Number(value) <= max),
      `${label} must be a number between ${min} and ${max}`
    )

const optionalInt = (label: string, min: number, max: number) =>
  z
    .string()
    .trim()
    .refine(
      (value) =>
        value === '' ||
        (/^\d+$/.test(value) && Number(value) >= min && Number(value) <= max),
      `${label} must be a whole number between ${min} and ${max}`
    )

/** Ports typed as `3000, 8080`; empty means none. */
export function parsePorts(raw: string): number[] | null {
  const parts = raw
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean)
  const ports: number[] = []
  for (const part of parts) {
    if (!/^\d+$/.test(part)) return null
    const port = Number(part)
    if (port < 1 || port > 65_535) return null
    if (!ports.includes(port)) ports.push(port)
  }
  return ports
}

export const createSandboxFormSchema = z.object({
  name: z.string().trim().max(64, 'Name must be 64 characters or fewer'),
  lifecycle: z.enum(['ephemeral', 'workspace']),
  image: z
    .string()
    .trim()
    .refine((value) => !/\s/.test(value), 'Image cannot contain spaces'),
  timeoutMinutes: optionalInt(
    'Idle timeout',
    SANDBOX_TIMEOUT_MIN_SECS / 60,
    SANDBOX_TIMEOUT_MAX_SECS / 60
  ),
  cpuLimit: optionalNumber('CPU limit', 0.1, 64),
  memoryLimitMb: optionalInt('Memory limit', 64, 262_144),
  pidsLimit: optionalInt('Process limit', 16, 1_000_000),
  ports: z
    .string()
    .trim()
    .refine(
      (value) => parsePorts(value) !== null,
      'Ports must be numbers from 1 to 65535, separated by commas'
    ),
  node: z.string(),
  previewPassword: z
    .string()
    .refine(
      (value) =>
        value === '' ||
        (value.length >= PREVIEW_PASSWORD_MIN_LEN &&
          value.length <= PREVIEW_PASSWORD_MAX_LEN),
      `Preview password must be ${PREVIEW_PASSWORD_MIN_LEN}-${PREVIEW_PASSWORD_MAX_LEN} characters`
    ),
})

export type CreateSandboxFormValues = z.infer<typeof createSandboxFormSchema>

export const createSandboxFormDefaults: CreateSandboxFormValues = {
  name: '',
  lifecycle: 'ephemeral',
  image: '',
  timeoutMinutes: '',
  cpuLimit: '',
  memoryLimitMb: '',
  pidsLimit: '',
  ports: '',
  node: AUTO_NODE,
  previewPassword: '',
}

/**
 * Map the form to the request body, sending only what the operator filled in
 * so every omitted field keeps the platform default.
 */
export function toCreateSandboxBody(
  values: CreateSandboxFormValues
): CreateSandboxBody {
  const body: CreateSandboxBody = { lifecycle: values.lifecycle }
  if (values.name.trim()) body.name = values.name.trim()
  if (values.image.trim()) body.image = values.image.trim()
  if (values.timeoutMinutes.trim()) {
    body.timeout_secs = Number(values.timeoutMinutes) * 60
  }
  if (values.cpuLimit.trim()) body.cpu_limit = Number(values.cpuLimit)
  if (values.memoryLimitMb.trim()) {
    body.memory_limit_mb = Number(values.memoryLimitMb)
  }
  if (values.pidsLimit.trim()) body.pids_limit = Number(values.pidsLimit)
  const ports = parsePorts(values.ports)
  if (ports && ports.length > 0) body.ports = ports
  if (values.node && values.node !== AUTO_NODE) body.node = values.node
  if (values.previewPassword) body.preview_password = values.previewPassword
  return body
}
