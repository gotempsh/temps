// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Download, Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import type { HostImagePreparation } from '@/api/client'
import {
  getSandboxImagePreparationOptions,
  getSandboxImagePreparationQueryKey,
  prepareSandboxImagesMutation,
  updateHostImagePreparationMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { problemDetail } from '@/lib/api-problem'
import {
  anyHostPulling,
  downloadLabel,
  downloadPercent,
  hostsToPrepare,
  imageStateLabel,
} from './sandbox-creation'

/** Polling while a host pulls; the download moves every second or two. */
const PULLING_POLL_MS = 2_000
/** Polling otherwise: catches a pull another admin or an upgrade started. */
const IDLE_POLL_MS = 30_000

type BadgeVariant = 'default' | 'secondary' | 'destructive' | 'outline'

function stateVariant(host: HostImagePreparation): BadgeVariant {
  switch (host.status?.state) {
    case 'ready':
      return 'default'
    case 'failed':
      return 'destructive'
    case 'pulling':
      return 'secondary'
    default:
      return 'outline'
  }
}

/**
 * Whether each sandbox host already has the default sandbox image, so the
 * first sandbox there starts in seconds instead of downloading several
 * gigabytes. Administrators start the pull ("Prepare sandboxes") and choose
 * per host whether Temps keeps it prepared across upgrades; everyone else
 * sees the status.
 */
export function SandboxImagePreparationCard({
  canManage,
}: {
  canManage: boolean
}) {
  const queryClient = useQueryClient()
  const status = useQuery({
    ...getSandboxImagePreparationOptions(),
    refetchInterval: (query) =>
      anyHostPulling(query.state.data?.hosts ?? [])
        ? PULLING_POLL_MS
        : IDLE_POLL_MS,
  })
  const queryKey = getSandboxImagePreparationQueryKey()

  const prepare = useMutation({
    ...prepareSandboxImagesMutation(),
    meta: { errorTitle: 'Failed to prepare sandbox hosts' },
    onSuccess: (data) => {
      const hosts = data.node_ids.length
      toast.success(
        `Pulling the sandbox image on ${hosts} ${hosts === 1 ? 'host' : 'hosts'} in the background`
      )
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey }),
  })

  const keep = useMutation({
    ...updateHostImagePreparationMutation(),
    meta: { errorTitle: 'Failed to update the host' },
    onSuccess: (data, variables) => {
      queryClient.setQueryData(queryKey, data)
      toast.success(
        variables.body.keep_prepared
          ? 'This host will be kept prepared'
          : 'This host will no longer be kept prepared'
      )
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey }),
  })

  const hosts = status.data?.hosts ?? []
  const pending = hostsToPrepare(hosts)

  return (
    <Card>
      <CardHeader className="flex flex-col gap-2 space-y-0 pb-3 sm:flex-row sm:items-center sm:justify-between">
        <CardTitle className="text-sm font-medium">Sandbox image</CardTitle>
        {canManage && pending.length > 0 && (
          <Button
            size="sm"
            variant={status.data?.all_ready === false ? 'default' : 'outline'}
            disabled={prepare.isPending}
            onClick={() => prepare.mutate({ body: {} })}
          >
            {prepare.isPending ? (
              <Loader2 className="mr-1.5 size-4 animate-spin" />
            ) : (
              <Download className="mr-1.5 size-4" />
            )}
            Prepare sandboxes
          </Button>
        )}
      </CardHeader>
      <CardContent className="space-y-3">
        <PreparationBody
          loading={status.isLoading}
          error={
            status.isError
              ? problemDetail(
                  status.error,
                  'The sandbox hosts could not be read.'
                )
              : null
          }
          onRetry={() => void status.refetch()}
          hosts={hosts}
          pendingCount={pending.length}
          canManage={canManage}
          keepPending={keep.isPending}
          onKeepChange={(host, value) =>
            keep.mutate({
              path: { node_id: host.node_id },
              body: { keep_prepared: value },
            })
          }
        />
      </CardContent>
    </Card>
  )
}

function PreparationBody({
  loading,
  error,
  onRetry,
  hosts,
  pendingCount,
  canManage,
  keepPending,
  onKeepChange,
}: {
  loading: boolean
  error: string | null
  onRetry: () => void
  hosts: HostImagePreparation[]
  pendingCount: number
  canManage: boolean
  keepPending: boolean
  onKeepChange: (host: HostImagePreparation, value: boolean) => void
}) {
  if (loading) {
    return (
      <div className="space-y-2" aria-busy="true">
        <Skeleton className="h-4 w-2/3" />
        <Skeleton className="h-9 w-full" />
      </div>
    )
  }
  const refreshFailed = error ? (
    <Alert variant="destructive">
      <AlertTriangle className="size-4" />
      <AlertDescription className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <span className="[overflow-wrap:anywhere]">{error}</span>
        <Button variant="outline" size="sm" onClick={onRetry}>
          Try again
        </Button>
      </AlertDescription>
    </Alert>
  ) : null
  // A failed refresh keeps the last hosts read on screen, under the error:
  // a pull in progress should not vanish because one poll failed.
  if (refreshFailed && hosts.length === 0) {
    return refreshFailed
  }
  if (hosts.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No host can run sandboxes yet.
      </p>
    )
  }
  return (
    <>
      {refreshFailed}
      {pendingCount > 0 && (
        <p className="text-sm text-muted-foreground">
          {canManage
            ? 'The first sandbox on a host without the image downloads it (several GB) before it starts. Prepare the hosts now to start it in seconds.'
            : 'The first sandbox on a host without the image downloads it (several GB) before it starts. An administrator can prepare the hosts ahead of time.'}
        </p>
      )}
      <ul className="divide-y rounded-md border">
        {hosts.map((host) => (
          <HostRow
            key={host.node_id}
            host={host}
            canManage={canManage}
            keepPending={keepPending}
            onKeepChange={onKeepChange}
          />
        ))}
      </ul>
    </>
  )
}

function HostRow({
  host,
  canManage,
  keepPending,
  onKeepChange,
}: {
  host: HostImagePreparation
  canManage: boolean
  keepPending: boolean
  onKeepChange: (host: HostImagePreparation, value: boolean) => void
}) {
  const s = host.status
  const percent = downloadPercent(s?.bytes_done, s?.bytes_total)
  const bytes = downloadLabel(s?.bytes_done, s?.bytes_total)
  const switchId = `keep-prepared-${host.node_id}`
  const problem =
    s?.state === 'failed' || s?.state === 'unsupported'
      ? s.error
      : host.status_error
  return (
    <li className="space-y-2 p-3" data-host={host.node_id}>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0 space-y-0.5">
          <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
            <span className="[overflow-wrap:anywhere]">{host.node_name}</span>
            {host.eligible ? (
              <Badge variant={stateVariant(host)}>{imageStateLabel(s)}</Badge>
            ) : (
              <Badge variant="outline">Not running sandboxes</Badge>
            )}
          </div>
          {s?.image && (
            <p className="font-mono text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {s.image}
            </p>
          )}
          {!host.eligible && host.ineligible_reason && (
            <p className="text-xs text-muted-foreground">
              {host.ineligible_reason}
            </p>
          )}
        </div>
        {host.eligible && (
          <div className="flex items-center gap-2">
            <Switch
              id={switchId}
              checked={host.keep_prepared}
              disabled={!canManage || keepPending}
              onCheckedChange={(value) => onKeepChange(host, value)}
            />
            <Label
              htmlFor={switchId}
              className="text-xs font-normal text-muted-foreground"
            >
              Keep prepared
            </Label>
          </div>
        )}
      </div>
      {s?.state === 'pulling' && (
        <div className="space-y-1">
          {percent !== null && (
            <Progress
              value={percent}
              className="h-1.5"
              aria-label={`${host.node_name} download`}
            />
          )}
          <p className="text-xs text-muted-foreground tabular-nums">
            {[s.detail, bytes].filter(Boolean).join(' · ') || 'Starting'}
          </p>
        </div>
      )}
      {problem && (
        <p className="text-xs text-destructive [overflow-wrap:anywhere]">
          {problem}
        </p>
      )}
      {host.stale_images.length > 0 && (
        <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
          {host.stale_images.length === 1
            ? 'A previous image is'
            : `${host.stale_images.length} previous images are`}{' '}
          still used by a sandbox and removed once nothing uses them:{' '}
          <span className="font-mono">{host.stale_images.join(', ')}</span>
        </p>
      )}
    </li>
  )
}
