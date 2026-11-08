// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  AccessServiceResponse,
  CreateAccessServiceData,
  ServiceGrantStatus,
} from '@/api/client'

export const accessServiceSchema = z
  .object({
    name: z.string().trim().min(1).max(80),
    origin: z.url().refine((value) => {
      try {
        const url = new URL(value)
        return (
          url.protocol === 'https:' &&
          !url.username &&
          !url.password &&
          !url.port &&
          url.pathname === '/' &&
          !url.search &&
          !url.hash
        )
      } catch {
        return false
      }
    }, 'Use an HTTPS origin without a path or credentials'),
    paths: z.string().trim().min(1),
    methods: z
      .array(z.enum(['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE']))
      .min(1),
    maxTtl: z.number().int().min(60).max(86400),
    enabled: z.boolean(),
    credential: z.string().max(16384),
    clearCredential: z.boolean(),
  })
  .refine((value) => !(value.credential && value.clearCredential), {
    message: 'Choose a new credential or remove the current one',
    path: ['credential'],
  })
export type AccessServiceValues = z.infer<typeof accessServiceSchema>

export function serviceDefaults(
  service?: AccessServiceResponse
): AccessServiceValues {
  return {
    name: service?.name ?? '',
    origin: service?.policy.origin ?? '',
    paths: service?.policy.path_prefixes.join('\n') ?? '/',
    methods: service?.policy.methods ?? ['GET', 'HEAD'],
    maxTtl: service?.max_ttl_seconds ?? 900,
    enabled: service?.enabled ?? true,
    credential: '',
    clearCredential: false,
  }
}
export function serviceBody(
  values: AccessServiceValues,
  version?: number
): CreateAccessServiceData['body'] {
  return {
    name: values.name,
    policy: {
      origin: values.origin,
      path_prefixes: values.paths
        .split('\n')
        .map((path) => path.trim())
        .filter(Boolean),
      methods: values.methods,
    },
    max_ttl_seconds: values.maxTtl,
    enabled: values.enabled,
    credential: values.credential || undefined,
    clear_credential: values.clearCredential,
    expected_version: version,
  }
}
export const grantRequestSchema = z.object({
  ttl: z.number().int().min(60).max(86400),
})
export const grantLabels: Record<ServiceGrantStatus, string> = {
  integration_unavailable: 'Not connected',
  expired: 'Expired',
  revoked: 'Revoked',
  service_disabled: 'Service disabled',
  policy_changed: 'Policy changed',
}
