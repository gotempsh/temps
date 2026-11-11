// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  AccessServiceResponse,
  CreateAccessServiceData,
  CredentialDelivery,
  ServiceCatalogEntry,
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
    catalogId: z.string().optional(),
    provider: z.enum([
      'custom',
      'github',
      'gitlab',
      'openai',
      'anthropic',
      'stripe',
      'resend',
      'sendgrid',
      'vercel',
      'netlify',
      'mistral',
      'groq',
    ]),
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
    /** Account name for services that sign in with a username and password. */
    loginUsername: z.string().max(256),
    /** How the credential is sent and which fixed headers go with it. */
    delivery: z.custom<CredentialDelivery>().optional(),
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

export function serviceEditorSchema(
  service: AccessServiceResponse | undefined,
  source: 'bearer' | 'api' | 'git'
) {
  return accessServiceSchema.superRefine((values, context) => {
    if (source !== 'bearer' && !values.githubRepository) {
      context.addIssue({
        code: 'custom',
        path: ['githubRepository'],
        message: 'Select a GitHub connection and repository',
      })
    }
    if (
      source === 'bearer' &&
      values.enabled &&
      !values.credential &&
      !(
        service?.has_credential &&
        !service.policy.github_repository &&
        !values.clearCredential
      )
    ) {
      context.addIssue({
        code: 'custom',
        path: ['credential'],
        message: 'Provide a bearer credential or disable access requests',
      })
    }
    const headers = values.delivery?.headers ?? []
    if (headers.some((header) => !/^[\x20-\x7e]{1,256}$/.test(header.value))) {
      context.addIssue({
        code: 'custom',
        path: ['delivery'],
        message: `Enter a value for ${headers
          .filter((header) => !/^[\x20-\x7e]{1,256}$/.test(header.value))
          .map((header) => header.name)
          .join(', ')}`,
      })
    }
    if (isLogin(values.delivery) && values.credential) {
      if (!values.loginUsername || values.loginUsername.includes(':')) {
        context.addIssue({
          code: 'custom',
          path: ['loginUsername'],
          message: 'Enter the username, without a colon',
        })
      }
    }
    if (source === 'bearer' && values.origin === 'https://github.com') {
      context.addIssue({
        code: 'custom',
        path: ['credential'],
        message:
          'For GitHub clone/fetch, select the GitHub clone/fetch credential source and a repository',
      })
    }
  })
}

/** True when the stored credential is a username and password pair. */
export function isLogin(delivery?: CredentialDelivery): boolean {
  return delivery?.format?.method === 'basic'
}

export function serviceDefaults(
  service?: AccessServiceResponse,
  catalogEntry?: ServiceCatalogEntry
): AccessServiceValues {
  return {
    name: service?.name ?? '',
    provider:
      service?.policy.provider ??
      (service?.policy.github_repository ? 'github' : 'custom'),
    origin: service?.policy.origin ?? '',
    paths: service?.policy.path_prefixes.join('\n') ?? '/',
    methods: service?.policy.methods ?? ['GET', 'HEAD'],
    maxTtl: service?.max_ttl_seconds ?? 900,
    enabled: service?.enabled ?? true,
    credential: '',
    loginUsername: '',
    delivery: service
      ? (service.policy.delivery ?? undefined)
      : (catalogEntry?.delivery ?? undefined),
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
      provider: values.provider === 'custom' ? undefined : values.provider,
      path_prefixes: pathPrefixes(values.paths),
      methods: values.methods,
      github_repository: values.githubRepository,
      delivery: values.githubRepository ? undefined : values.delivery,
    },
    max_ttl_seconds: values.maxTtl,
    enabled: values.enabled,
    credential: values.credential
      ? isLogin(values.delivery)
        ? `${values.loginUsername}:${values.credential}`
        : values.credential
      : undefined,
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
