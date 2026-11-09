// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useState } from 'react'
import { Link, useParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  DataTable,
  Detail,
  EchoDialog,
  PageContainer,
  Status,
} from '@temps-sdk/ds'
import { revokeAccessSession } from '@/api/client'
import {
  accessCapabilityOptions,
  getAccessSessionOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceProviderLogo } from '@/components/service-access/service-providers'
import { useSessionClock } from '@/components/service-access/session-navigation'
import { SessionStatus } from '@/components/service-access/AccessSessionsList'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

export default function AccessSessionDetailPage() {
  const { projectId = '', vaultId = '', sessionId = '' } = useParams()
  return (
    <SessionDetail
      key={`${projectId}:${vaultId}:${sessionId}`}
      projectId={Number(projectId)}
      vaultId={vaultId}
      sessionId={sessionId}
    />
  )
}
function SessionDetail({
  projectId,
  vaultId,
  sessionId,
}: {
  projectId: number
  vaultId: string
  sessionId: string
}) {
  const valid =
    Number.isInteger(projectId) &&
    projectId > 0 &&
    projectId <= 2147483647 &&
    !!sessionId &&
    !!vaultId
  const base = `/vault/projects/${projectId}/vaults/${encodeURIComponent(vaultId)}`
  const back = `${base}?tab=sessions`
  const query = useQuery({
    ...getAccessSessionOptions({
      path: { project_id: projectId, session_id: sessionId },
    }),
    enabled: valid,
    refetchInterval: 3000,
  })
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId } }),
    enabled: valid,
  })
  const [confirming, setConfirming] = useState(false)
  const client = useQueryClient()
  const revoke = useMutation({
    mutationFn: () =>
      revokeAccessSession({
        path: { project_id: projectId, session_id: sessionId },
        throwOnError: true,
      }),
    onSuccess: () => {
      setConfirming(false)
      void query.refetch()
      void client.invalidateQueries({
        queryKey: [{ _id: 'listAccessSessions' }],
      })
      void client.invalidateQueries({
        queryKey: [{ _id: 'listServiceGrants' }],
      })
    },
  })
  const session = query.data?.session
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vault', href: '/vault' },
      { label: 'Vault', href: base },
      { label: 'Sessions', href: back },
      { label: session?.bundle_name ?? 'Session' },
    ])
    return () => setBreadcrumbs([])
  }, [setBreadcrumbs, base, back, session?.bundle_name])
  const mismatch = session && session.vault_id !== vaultId
  const now = useSessionClock()
  const issued =
    session?.status === 'issued' && Date.parse(session.expires_at) > now
  return (
    <PageContainer width="full">
      {!valid || mismatch ? (
        <Callout tone="error" title="Invalid session link">
          <Link to="/vault">Back to Vault</Link>
        </Callout>
      ) : (
        <>
          {query.isError && (
            <Callout tone="error" title="Could not refresh session">
              {problemDetail(
                query.error,
                'Worker status could not be confirmed.'
              )}
              <Button onClick={() => void query.refetch()}>Retry</Button>
            </Callout>
          )}
          {!session ? (
            !query.isError && <Skeleton className="h-64" />
          ) : (
            <Detail
              embedded
              title={session.bundle_name}
              verdict={<SessionStatus session={session} />}
              actions={
                <div className="flex flex-wrap gap-2">
                  <Button variant="outline" asChild>
                    <Link to={back}>All sessions</Link>
                  </Button>
                  <Button
                    variant="destructive"
                    disabled={
                      !issued ||
                      !capability.data?.can_manage_requests ||
                      capability.isError ||
                      query.isError
                    }
                    onClick={() => setConfirming(true)}
                  >
                    Revoke session
                  </Button>
                </div>
              }
              facts={[
                {
                  label: 'Allowed consumer',
                  value: (
                    <Link
                      className="break-all underline"
                      to={`/sandboxes/${encodeURIComponent(session.consumer.sandbox_id)}`}
                    >
                      {session.consumer.sandbox_id}
                    </Link>
                  ),
                },
                {
                  label: 'Access bundle',
                  value: (
                    <Link
                      to={`${base}/bundles/${encodeURIComponent(session.bundle_id)}`}
                      className="underline"
                    >
                      {session.bundle_name} · v{session.bundle_version}
                    </Link>
                  ),
                },
                {
                  label: 'Issued',
                  value: new Date(session.created_at).toLocaleString(),
                },
                {
                  label: 'Hard expiry',
                  value: new Date(session.expires_at).toLocaleString(),
                },
              ]}
              main={
                <div className="space-y-4">
                  <p className="text-sm text-muted-foreground">
                    Only the selected sandbox can use these policies. Issued
                    means authorization was saved; worker status below confirms
                    enforcement.
                  </p>
                  {session.revoked_at && (
                    <Callout title="Revocation requested">
                      Access is being removed. Worker confirmation or signed
                      lease expiry completes revocation.
                    </Callout>
                  )}
                  {revoke.isError && (
                    <Callout tone="error" title="Could not revoke session">
                      {problemDetail(
                        revoke.error,
                        'Check your permissions and retry.'
                      )}
                    </Callout>
                  )}
                  <DataTable
                    aria-label="Session service restrictions"
                    rows={query.data?.grants ?? []}
                    rowKey={(g) => g.id}
                    columns={[
                      {
                        key: 'service',
                        header: 'Service',
                        render: (g) => (
                          <div className="flex items-start gap-3">
                            <ServiceProviderLogo service={g} />
                            <div className="min-w-0">
                              <p className="font-medium break-words">
                                {g.service_name}
                              </p>
                              <p className="text-xs text-muted-foreground break-all">
                                {g.policy.origin}
                              </p>
                              <p className="text-xs text-muted-foreground break-all @xl:hidden">
                                {g.policy.methods.join(', ')} ·{' '}
                                {g.policy.path_prefixes.join(', ')}
                              </p>
                            </div>
                          </div>
                        ),
                      },
                      {
                        key: 'scope',
                        header: 'Allowed requests',
                        className: 'hidden @xl:table-cell',
                        render: (g) => (
                          <div className="break-all text-sm">
                            <p>{g.policy.methods.join(', ')}</p>
                            <p className="font-mono text-xs text-muted-foreground">
                              {g.policy.path_prefixes.join(', ')}
                            </p>
                          </div>
                        ),
                      },
                      {
                        key: 'status',
                        header: 'Worker status',
                        render: (g) => (
                          <div className="space-y-1">
                            <Status
                              tone={
                                g.enforcement === 'active'
                                  ? 'ok'
                                  : g.enforcement === 'pending' ||
                                      g.enforcement === 'stopping'
                                    ? 'running'
                                    : 'idle'
                              }
                              label={g.enforcement}
                            />
                            <p className="text-xs text-muted-foreground">
                              {g.reason}
                            </p>
                          </div>
                        ),
                      },
                    ]}
                  />
                  <p className="break-all font-mono text-xs text-muted-foreground">
                    {session.id}
                  </p>
                </div>
              }
            />
          )}
          <EchoDialog
            open={confirming}
            onOpenChange={(next) => {
              if (!revoke.isPending) setConfirming(next)
            }}
            title="Revoke session"
            description="Remove every service authorization in this session. Other sessions keep their own access and deadlines."
            phrase={session?.bundle_name ?? 'session'}
            confirmLabel="Revoke session"
            busy={revoke.isPending}
            onConfirm={() => {
              if (
                issued &&
                capability.data?.can_manage_requests &&
                !revoke.isPending
              )
                revoke.mutate()
            }}
          />
        </>
      )}
    </PageContainer>
  )
}
