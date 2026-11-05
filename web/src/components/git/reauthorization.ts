// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ConnectionResponse,
  ProviderResponse,
} from '@/api/client/types.gen'

/** Whether this provider's authentication method has a Reauthorize action at all. */
export function providerSupportsReauthorization(
  provider?: Pick<ProviderResponse, 'auth_method' | 'provider_type'>
) {
  return (
    provider?.auth_method === 'oauth' &&
    ['github', 'gitlab'].includes(provider.provider_type)
  )
}

export function canReauthorizeConnection(
  connection: Pick<ConnectionResponse, 'installation_id' | 'user_id'>,
  provider?: Pick<ProviderResponse, 'auth_method' | 'provider_type'>,
  currentUserId?: number
) {
  return (
    currentUserId !== undefined &&
    connection.user_id === currentUserId &&
    providerSupportsReauthorization(provider) &&
    !connection.installation_id
  )
}

export function needsReauthorization(
  connection: Pick<ConnectionResponse, 'is_expired' | 'health_status'>
) {
  return connection.is_expired || connection.health_status === 'unhealthy'
}

/** Shown instead of whatever the provider answered, which is not for operators. */
export const GITLAB_AUTHORIZATION_REJECTED =
  "GitLab no longer accepts this connection's authorization"

/**
 * The message for a GitLab OAuth connection whose refresh token was rejected,
 * or `null` for anything else. `is_expired` is the block itself. The
 * `invalid_grant` check covers a health message recorded before the server
 * stopped storing raw provider responses, until the next check replaces it.
 */
export function authorizationRejectedMessage(
  connection: Pick<ConnectionResponse, 'is_expired' | 'health_message'>,
  provider?: Pick<ProviderResponse, 'auth_method' | 'provider_type'>
) {
  if (
    provider?.provider_type !== 'gitlab' ||
    ['pat', 'gitlab_pat'].includes(provider.auth_method)
  )
    return null
  return connection.is_expired ||
    connection.health_message?.includes('invalid_grant')
    ? GITLAB_AUTHORIZATION_REJECTED
    : null
}

export function readReauthorizationCallback(hash: string) {
  const params = new URLSearchParams(hash.replace(/^#/, ''))
  const state = params.get('reauthorize_state')
  if (!state) return null
  return { state, code: params.get('reauthorize_code') ?? '' }
}
