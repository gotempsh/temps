// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ConnectionListResponse,
  ConnectionResponse,
  ProviderResponse,
} from '@/api/client/types.gen'
import { isGitHubApp } from '@/lib/provider'

// An installation webhook adds a connection to an existing provider. Keep
// checking even when the list is empty or all existing connections are idle.
export function gitInstallationPollingOptions(
  providers: ProviderResponse[] | undefined
) {
  return {
    refetchInterval: (query: {
      state: { data?: ConnectionResponse[] | ConnectionListResponse }
    }): number | false => {
      const data = query.state.data
      const connections = Array.isArray(data) ? data : data?.connections
      return providers?.some(isGitHubApp) ||
        connections?.some((connection) => connection.syncing)
        ? 2000
        : false
    },
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: 'always' as const,
  }
}

export function hasNewGitHubInstallation(
  connections: ConnectionResponse[] | undefined,
  previousConnectionIds: number[],
  installationId: string | null
): boolean {
  return Boolean(
    connections?.some(
      (connection) =>
        connection.installation_id &&
        (installationId
          ? connection.installation_id === installationId
          : !previousConnectionIds.includes(connection.id))
    )
  )
}
