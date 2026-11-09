// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  AccessServiceResponse,
  CreateAccessServiceData,
  ServiceGrantStatus,
} from '@/api/client'

function pathPrefixes(value: string): string[] {
  return value
    .split('\n')
    .map((path) => path.trim())
    .filter(Boolean)
}

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
    paths: z
      .string()
      .trim()
      .superRefine((value, context) => {
        const prefixes = pathPrefixes(value)
        if (prefixes.length < 1 || prefixes.length > 32) {
          context.addIssue({
            code: 'custom',
            message: 'Provide between 1 and 32 path prefixes',
          })
          return
        }
        if (
          prefixes.some(
            (path) =>
              !path.startsWith('/') ||
              path.length > 512 ||
              path.includes('//') ||
              path
                .split('/')
                .some((segment) => segment === '.' || segment === '..') ||
              !/^[a-zA-Z0-9/_.~-]+$/.test(path)
          )
        ) {
          context.addIssue({
            code: 'custom',
            message:
              'Use absolute paths of at most 512 characters, without encoding, traversal, query strings or wildcards',
          })
        }
      }),
    methods: z
      .array(z.enum(['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE']))
      .min(1),
    maxTtl: z.number().int().min(60).max(86400),
    enabled: z.boolean(),
    credential: z.string().max(16384),
    clearCredential: z.boolean(),
    githubRepository: z
      .object({
        connection_id: z.number().int().positive(),
        owner: z.string().min(1),
        repository: z.string().min(1),
        protocol: z.enum(['api', 'git']),
      })
      .optional(),
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
    githubRepository: service?.policy.github_repository ?? undefined,
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
      path_prefixes: pathPrefixes(values.paths),
      methods: values.methods,
      github_repository: values.githubRepository,
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
export const enforcementLabels = {
  not_requested: 'Awaiting approval',
  pending: 'Connecting',
  active: 'Connected',
  stopping: 'Revoking',
  stopped: 'Disconnected',
} as const

export const grantLabels: Record<ServiceGrantStatus, string> = {
  pending_approval: 'Awaiting approval',
  approved: 'Approved',
  integration_unavailable: 'Not connected',
  expired: 'Expired',
  revoked: 'Revoked',
  service_disabled: 'Service disabled',
  policy_changed: 'Policy changed',
}
