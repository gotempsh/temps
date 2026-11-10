// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { sessionHref, useSessionClock } from './session-navigation'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Clock3 } from 'lucide-react'
import { Button, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  DataTable,
  PageState,
  ResponsivePagination,
  Status,
} from '@temps-sdk/ds'
import { listAccessSessionsGeneralOptions } from '@/api/client/@tanstack/react-query.gen'
import type { AccessSessionResponse } from '@/api/client'
import { problemDetail } from '@/lib/api-problem'

export function SessionStatus({ session }: { session: AccessSessionResponse }) {
  const now = useSessionClock()
  const expired =
    session.status === 'expired' ||
    (session.status === 'issued' && Date.parse(session.expires_at) <= now)
  const label =
    session.status === 'revoked' ? 'Revoked' : expired ? 'Expired' : 'Issued'
  return <Status tone={label === 'Issued' ? 'idle' : 'warn'} label={label} />
}
export function AccessSessionsList({
  vaultId,
  sandboxId,
  active = true,
}: {
  projectId?: number
  vaultId?: string
  sandboxId?: string
  active?: boolean
}) {
  const [page, setPage] = useState(1)
  const query = useQuery({
    ...listAccessSessionsGeneralOptions({
      query: { vault_id: vaultId, sandbox_id: sandboxId, page, page_size: 20 },
    }),
    enabled: active,
    refetchInterval: active ? 5000 : false,
  })
  return (
    <div className="space-y-4">
      {query.isError && (
        <Callout tone="error" title="Could not load sessions">
          {problemDetail(
            query.error,
            'Check your project permissions and retry.'
          )}
          <Button onClick={() => void query.refetch()}>Retry</Button>
        </Callout>
      )}
      {query.isPending ? (
        <Skeleton className="h-40" />
      ) : query.data?.total === 0 ? (
        <PageState
          variant="empty"
          icon={Clock3}
          title="No sessions"
          description={
            sandboxId
              ? 'Create a session from a Vault access bundle and choose this sandbox.'
              : 'Open an access bundle to issue time-limited access to a sandbox.'
          }
        />
      ) : (
        query.data && (
          <DataTable
            aria-label="Vault sessions"
            rows={query.data.items}
            rowKey={(s) => s.id}
            columns={[
              {
                key: 'bundle',
                header: 'Session',
                render: (s) => (
                  <div className="space-y-1">
                    <Link
                      className="font-medium break-words underline-offset-4 hover:underline"
                      to={sessionHref(s)}
                    >
                      {s.bundle_name}
                    </Link>
                    <p className="text-xs text-muted-foreground break-all">
                      {s.consumer.sandbox_id}
                    </p>
                    <p className="text-xs text-muted-foreground @xl:hidden">
                      Expires {new Date(s.expires_at).toLocaleString()}
                    </p>
                  </div>
                ),
              },
              {
                key: 'services',
                header: 'Services',
                className: 'hidden @xl:table-cell',
                render: (s) => s.service_count,
              },
              {
                key: 'expiry',
                header: 'Expires',
                className: 'hidden @xl:table-cell',
                render: (s) => (
                  <time dateTime={s.expires_at}>
                    {new Date(s.expires_at).toLocaleString()}
                  </time>
                ),
              },
              {
                key: 'state',
                header: 'Authorization',
                render: (s) => <SessionStatus session={s} />,
              },
            ]}
          />
        )
      )}
      {(query.data?.total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={query.data?.total ?? 0}
          totalPages={Math.ceil((query.data?.total ?? 0) / 20)}
          onPageChange={setPage}
        />
      )}
    </div>
  )
}
