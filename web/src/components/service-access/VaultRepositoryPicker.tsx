// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Callout, ResponsivePagination } from '@temps-sdk/ds'
import { Button } from '@temps-sdk/ui'
import {
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { RepositoryResponse } from '@/api/client'
import { GitConnectionRepositoryPicker } from '@/components/repositories/GitConnectionRepositoryPicker'
import { problemDetail } from '@/lib/api-problem'

export function VaultRepositoryPicker({
  connectionId,
  onConnectionChange,
  onRepositorySelect,
}: {
  connectionId: number | null
  onConnectionChange: (id: number) => void
  onRepositorySelect: (repository: RepositoryResponse) => void
}) {
  const [page, setPage] = useState(1)
  const connections = useQuery(
    listConnectionsOptions({ query: { page, per_page: 20 } })
  )
  const providers = useQuery(listGitProvidersOptions())
  const available =
    connections.data?.connections.filter(
      (connection) =>
        connection.is_active &&
        !connection.is_expired &&
        providers.data?.some(
          (provider) =>
            provider.id === connection.provider_id &&
            provider.is_active &&
            provider.provider_type === 'github'
        )
    ) ?? []
  if (connections.isError || providers.isError)
    return (
      <Callout tone="error" title="Could not load GitHub connections">
        {problemDetail(
          connections.error ?? providers.error,
          'Check Git connection and repository read permissions, then retry.'
        )}
        <Button
          type="button"
          variant="outline"
          onClick={() => {
            void connections.refetch()
            void providers.refetch()
          }}
        >
          Retry connections
        </Button>
      </Callout>
    )
  if (connections.isPending || providers.isPending)
    return <p role="status">Loading GitHub connections…</p>
  return (
    <div className="space-y-3">
      {available.length === 0 ? (
        <Callout title="No active GitHub connections on this page">
          Connect a GitHub account and sync its repositories in{' '}
          <a className="underline" href="/git-providers">
            Git providers
          </a>
          .
        </Callout>
      ) : (
        <GitConnectionRepositoryPicker
          connections={available}
          connectionId={connectionId}
          onConnectionChange={onConnectionChange}
          onRepositorySelect={onRepositorySelect}
        />
      )}
      {(connections.data?.total_count ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={connections.data?.total_count ?? 0}
          totalPages={Math.ceil((connections.data?.total_count ?? 0) / 20)}
          onPageChange={setPage}
          ariaLabel="Git connection pages"
        />
      )}
    </div>
  )
}
