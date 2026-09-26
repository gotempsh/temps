// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const DEPLOYMENT_TOKEN_PERMISSIONS = [
  {
    value: '*',
    label: 'Full access',
    description: 'Allow every deployment-token capability.',
  },
  {
    value: 'analytics:read',
    label: 'Read analytics',
    description: 'Read project analytics data.',
  },
  {
    value: 'events:write',
    label: 'Write events',
    description: 'Send analytics events.',
  },
  {
    value: 'visitors:enrich',
    label: 'Enrich visitors',
    description: 'Add server-side visitor properties.',
  },
  {
    value: 'emails:send',
    label: 'Send email',
    description: 'Send email for this project.',
  },
  {
    value: 'errors:read',
    label: 'Read errors',
    description: 'Read error tracking data.',
  },
  {
    value: 'ai_gateway:execute',
    label: 'Use AI Gateway',
    description: 'Make requests through AI Gateway.',
  },
  {
    value: 'flags:read',
    label: 'Read feature flags',
    description: 'Evaluate project feature flags.',
  },
  {
    value: 'blob:read',
    label: 'Read blobs',
    description: 'Read Blob storage objects.',
  },
  {
    value: 'blob:write',
    label: 'Write blobs',
    description: 'Create and update Blob storage objects.',
  },
  {
    value: 'blob:delete',
    label: 'Delete blobs',
    description: 'Delete Blob storage objects.',
  },
  { value: 'kv:read', label: 'Read KV', description: 'Read KV entries.' },
  {
    value: 'kv:write',
    label: 'Write KV',
    description: 'Create and update KV entries.',
  },
  { value: 'kv:delete', label: 'Delete KV', description: 'Delete KV entries.' },
] as const

export function validateDeploymentTokenInput(
  name: string,
  expiresAt: string,
  permissions: string[]
) {
  if (!name.trim()) return 'Name is required.'
  if (expiresAt) {
    const expirationTime = new Date(expiresAt).getTime()
    if (!Number.isFinite(expirationTime) || expirationTime <= Date.now()) {
      return 'Expiration must be in the future.'
    }
  }
  if (permissions.length === 0) return 'Select at least one permission.'
  return null
}

export function deploymentTokenErrorMessage(error: unknown): string {
  if (typeof error === 'object' && error !== null) {
    const problem = error as { detail?: unknown; message?: unknown }
    if (typeof problem.detail === 'string' && problem.detail)
      return problem.detail
    if (typeof problem.message === 'string' && problem.message)
      return problem.message
  }
  return 'Failed to create deployment token.'
}
