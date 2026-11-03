// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ConnectionResponse,
  ProviderResponse,
} from '@/api/client/types.gen'

export function canReauthorizeConnection(
  connection: Pick<ConnectionResponse, 'installation_id'>,
  provider?: Pick<ProviderResponse, 'auth_method' | 'provider_type'>
) {
  return (
    provider?.auth_method === 'oauth' &&
    ['github', 'gitlab'].includes(provider.provider_type) &&
    !connection.installation_id
  )
}

export function needsReauthorization(
  connection: Pick<ConnectionResponse, 'is_expired' | 'health_status'>
) {
  return connection.is_expired || connection.health_status === 'unhealthy'
}

export function readReauthorizationCallback(hash: string) {
  const params = new URLSearchParams(hash.replace(/^#/, ''))
  const state = params.get('reauthorize_state')
  if (!state) return null
  return { state, code: params.get('reauthorize_code') ?? '' }
}
