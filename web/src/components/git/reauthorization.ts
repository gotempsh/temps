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

/**
 * The health to show for a connection. A rejected refresh sets `is_expired`
 * during a sync or deploy and `health_status` only moves at the next health
 * check, so an expired connection counts as unhealthy whatever the last one said.
 */
export function displayedHealthStatus(
  connection: Pick<ConnectionResponse, 'is_expired' | 'health_status'>
) {
  return connection.is_expired ? 'unhealthy' : connection.health_status
}

/** Shown instead of whatever the provider answered, which is not for operators. */
export const GITLAB_AUTHORIZATION_REJECTED =
  "GitLab no longer accepts this connection's authorization"

/**
 * Where a connection whose provider has no Reauthorize action gets a working
 * authorization: this flow adds a GitLab OAuth provider.
 */
export const GITLAB_OAUTH_SETUP_PATH = '/git-providers/add'

/**
 * The message for a GitLab connection whose refresh token was rejected, or
 * `null` for anything else. Check `providerSupportsReauthorization` before
 * offering Reauthorize with it, because a GitLab App connection reaches this
 * too and has no such action. `is_expired` is the block itself. The
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
