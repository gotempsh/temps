// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A tunnel that gives every environment an address under its domain
// (ADR 068 D3). Turning it on or off changes public DNS for every
// environment at once, so both directions list the hosts and ask first.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  getConnectorEnvironmentHostsOptions,
  getPreviewDomainStatusQueryKey,
  listIngressConnectorsOptions,
  listIngressHostnamesOptions,
  setConnectorEnvironmentHostsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ConnectorView,
  EnvironmentHostsResponse,
  EnvironmentHostView,
} from '@/api/client/types.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button, buttonVariants } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import { cn } from '@/lib/utils'

function HostState({ host }: { host: EnvironmentHostView }) {
  switch (host.state) {
    case 'published':
      return <Badge className="bg-green-600 hover:bg-green-600">Live</Badge>
    case 'error':
      return <Badge variant="destructive">Failed</Badge>
    case 'planned':
      return <Badge variant="outline">Once on</Badge>
    default:
      return <Badge variant="secondary">Publishing…</Badge>
  }
}

function HostList({ hosts }: { hosts: EnvironmentHostView[] }) {
  if (hosts.length === 0) {
    return (
      <p className="text-xs text-muted-foreground">
        No environment to publish yet.
      </p>
    )
  }
  return (
    <ul className="max-h-64 divide-y overflow-y-auto text-sm">
      {hosts.map((host) => (
        <li
          key={`${host.environment_id}-${host.hostname}`}
          className="flex flex-col gap-0.5 py-1.5"
        >
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-xs">{host.hostname}</span>
            <HostState host={host} />
            <span className="text-xs text-muted-foreground">
              {host.project_slug} / {host.environment_name}
            </span>
          </div>
          {host.note && (
            <span className="text-xs text-muted-foreground">{host.note}</span>
          )}
          {host.last_error && (
            <span className="text-xs text-destructive">{host.last_error}</span>
          )}
        </li>
      ))}
    </ul>
  )
}

export function TunnelEnvironmentHostsView({
  data,
  pending,
  onConfirm,
}: {
  data: EnvironmentHostsResponse
  pending: boolean
  onConfirm: (enabled: boolean) => void
}) {
  const [asking, setAsking] = useState<boolean | null>(null)
  const blocked = !data.enabled && Boolean(data.enabled_elsewhere)
  return (
    <div className="space-y-2 border-t pt-2">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-0.5 text-sm">
          <p className="font-medium">
            {data.enabled
              ? `Every environment has an address under ${data.domain}`
              : `Give every environment an address under ${data.domain}`}
          </p>
          <p className="text-xs text-muted-foreground">
            {blocked
              ? `Tunnel ${data.enabled_elsewhere} already does this. Only one tunnel can.`
              : data.preview_domain_usable && data.enabled
                ? 'The preview domain is usable, so new environments get no tunnel address; existing ones keep theirs.'
                : 'Apps become reachable without a wildcard domain: each environment is published as <environment>.' +
                  data.domain +
                  '.'}
          </p>
        </div>
        <Button
          size="sm"
          variant={data.enabled ? 'outline' : 'default'}
          className="w-full sm:w-auto"
          disabled={pending || blocked}
          onClick={() => setAsking(!data.enabled)}
        >
          {data.enabled ? 'Stop publishing environments' : 'Publish environments'}
        </Button>
      </div>
      {data.enabled && <HostList hosts={data.hosts} />}
      <AlertDialog
        open={asking !== null}
        onOpenChange={(open) => !open && setAsking(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {asking
                ? `Publish every environment through ${data.connector_name}?`
                : `Withdraw every environment address from ${data.connector_name}?`}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {asking
                ? data.warning
                : 'These addresses stop working and their DNS records are removed. Domains you added yourself are not touched.'}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <HostList hosts={data.hosts} />
          {asking && data.refused.length > 0 && (
            <div className="space-y-1 text-xs text-muted-foreground">
              <p className="font-medium">Not published:</p>
              <ul className="list-disc pl-4">
                {data.refused.map((r) => (
                  <li key={r.environment_id}>
                    {r.project_slug} / {r.environment_name}: {r.reason}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className={cn(
                !asking && buttonVariants({ variant: 'destructive' })
              )}
              onClick={() => {
                if (asking !== null) onConfirm(asking)
                setAsking(null)
              }}
            >
              {asking ? 'Publish them' : 'Withdraw them'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

/**
 * What deleting a tunnel withdraws with it (ADR 068 D3): its environment
 * hosts, listed so the confirmation names them.
 */
export function WithdrawnEnvironmentHosts({
  connector,
}: {
  connector: ConnectorView
}) {
  const query = useQuery({
    ...getConnectorEnvironmentHostsOptions({ path: { id: connector.id } }),
    enabled: connector.environment_hosts,
    retry: false,
  })
  if (!connector.environment_hosts) return null
  if (query.isPending) return <Skeleton className="h-10 w-full" />
  if (query.isError) {
    return (
      <p className="text-xs text-muted-foreground">
        Every environment address it publishes is withdrawn with it.
      </p>
    )
  }
  return (
    <div className="space-y-1">
      <p className="text-sm">
        These environment addresses stop working with it:
      </p>
      <HostList hosts={query.data.hosts} />
    </div>
  )
}

/** The opt-in for one tunnel, under its hostnames. */
export function TunnelEnvironmentHosts({
  connector,
}: {
  connector: ConnectorView
}) {
  const queryClient = useQueryClient()
  const options = getConnectorEnvironmentHostsOptions({
    path: { id: connector.id },
  })
  const query = useQuery({ ...options, refetchInterval: 15_000, retry: false })
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const set = useMutation({
    ...setConnectorEnvironmentHostsMutation(),
    onSuccess: (data) => {
      queryClient.setQueryData(options.queryKey, data)
      toast.success(
        data.enabled
          ? `Publishing every environment through ${data.connector_name}`
          : `Withdrew the environment addresses from ${data.connector_name}`
      )
      return Promise.all([
        queryClient.invalidateQueries({
          queryKey: listIngressConnectorsOptions().queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: listIngressHostnamesOptions().queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: getPreviewDomainStatusQueryKey(),
        }),
      ])
    },
    onError: (error, variables) => {
      // Turning it on asks for a recent verification first.
      if (handleSensitiveActionError(error, () => set.mutate(variables))) return
      // The list changed since it was shown: show the new one to confirm.
      if ((error as { status?: number } | null)?.status === 409) {
        void queryClient.invalidateQueries({ queryKey: options.queryKey })
      }
      toast.error('Could not change the environment addresses', {
        description: problemDetail(error, 'Try again in a moment.'),
      })
    },
  })
  if (query.isPending) {
    return <Skeleton className="h-10 w-full" />
  }
  if (query.isError) {
    return (
      <p className="border-t pt-2 text-xs text-muted-foreground">
        {problemDetail(
          query.error,
          'Could not load the environment addresses for this tunnel.'
        )}
      </p>
    )
  }
  const data = query.data
  return (
    <>
      <TunnelEnvironmentHostsView
        data={data}
        pending={set.isPending}
        onConfirm={(enabled) =>
          set.mutate({
            path: { id: connector.id },
            // Binds the change to the list that was shown.
            body: { enabled, confirm: data.confirm_token },
          })
        }
      />
      {verificationDialog}
    </>
  )
}
