// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Which hostnames go through which tunnel, and why (ADR 058 D9). The
// control plane decides; these only show its reasoning and record the
// operator's choice.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ExternalLink } from 'lucide-react'
import { toast } from 'sonner'
import {
  listIngressConnectorsOptions,
  listIngressHostnamesOptions,
  setIngressHostnameChoiceMutation,
  updateIngressConnectorMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ConnectorView, HostnameView } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { problemDetail } from '@/lib/api-problem'

function useHostnames() {
  return useQuery({
    ...listIngressHostnamesOptions(),
    refetchInterval: 15_000,
    retry: false,
  })
}

function useRefreshIngress() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({
        queryKey: listIngressHostnamesOptions().queryKey,
      }),
      queryClient.invalidateQueries({
        queryKey: listIngressConnectorsOptions().queryKey,
      }),
    ])
}

export function StateBadge({ view }: { view: HostnameView }) {
  switch (view.state) {
    case 'published':
      return <Badge className="bg-green-600 hover:bg-green-600">Live</Badge>
    case 'pending':
      return <Badge variant="secondary">Publishing…</Badge>
    case 'error':
      return <Badge variant="destructive">Failed</Badge>
    default:
      return <Badge variant="outline">Not through a tunnel</Badge>
  }
}

/** Automatic, a specific tunnel, or no tunnel, for one hostname. */
export function HostnameChoice({ view }: { view: HostnameView }) {
  const refresh = useRefreshIngress()
  const choose = useMutation({
    ...setIngressHostnameChoiceMutation(),
    onSuccess: refresh,
    onError: (error) =>
      toast.error(`Could not change ${view.hostname}`, {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })
  const value =
    view.mode === 'tunnel' && view.chosen_connector_id != null
      ? `tunnel:${view.chosen_connector_id}`
      : view.mode
  const automatic = view.candidates
    .filter((c) => c.auto_publish)
    .sort(
      (a, b) =>
        b.domain.length - a.domain.length || a.connector_id - b.connector_id
    )[0]
  return (
    <Select
      value={value}
      disabled={choose.isPending}
      onValueChange={(next) => {
        const [mode, id] = next.split(':')
        choose.mutate({
          path: { hostname: view.hostname },
          body:
            mode === 'tunnel'
              ? { mode: 'tunnel', connector_id: Number(id) }
              : { mode: mode as 'automatic' | 'off' },
        })
      }}
    >
      <SelectTrigger
        className="h-8 w-[220px] text-xs"
        aria-label={`How ${view.hostname} reaches visitors`}
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="automatic">
          Automatic{automatic ? ` (${automatic.name})` : ' (none)'}
        </SelectItem>
        {view.candidates.map((c) => (
          <SelectItem key={c.connector_id} value={`tunnel:${c.connector_id}`}>
            Through {c.name}
          </SelectItem>
        ))}
        <SelectItem value="off">Not through a tunnel</SelectItem>
      </SelectContent>
    </Select>
  )
}

export function HostnameRow({ view }: { view: HostnameView }) {
  return (
    <li className="space-y-1 py-1.5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          {view.state === 'published' ? (
            <a
              className="inline-flex items-center gap-1 break-all text-sm hover:underline"
              href={`https://${view.hostname}`}
              target="_blank"
              rel="noreferrer"
            >
              {view.hostname} <ExternalLink className="h-3 w-3 shrink-0" />
            </a>
          ) : (
            <span className="break-all text-sm">{view.hostname}</span>
          )}
          <StateBadge view={view} />
        </div>
        <HostnameChoice view={view} />
      </div>
      {view.state !== 'published' && (
        <p className="text-xs text-muted-foreground">{view.detail}</p>
      )}
    </li>
  )
}

/**
 * A tunnel's hostnames: the ones it publishes and the ones it could, with a
 * switch between publishing everything under its domain and only what the
 * operator picks.
 */
export function TunnelHostnames({ connector }: { connector: ConnectorView }) {
  const hostnamesQuery = useHostnames()
  const refresh = useRefreshIngress()
  const update = useMutation({
    ...updateIngressConnectorMutation(),
    onSuccess: refresh,
    onError: (error) =>
      toast.error('Could not change what the tunnel publishes', {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })
  const hosts = (hostnamesQuery.data ?? []).filter(
    (h) =>
      h.connector_id === connector.id ||
      h.candidates.some((c) => c.connector_id === connector.id)
  )
  return (
    <div className="space-y-1 border-t pt-2">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span className="text-muted-foreground">Publishes</span>
        <Select
          value={connector.auto_publish ? 'all' : 'chosen'}
          disabled={update.isPending}
          onValueChange={(next) =>
            update.mutate({
              path: { id: connector.id },
              body: { auto_publish: next === 'all' },
            })
          }
        >
          <SelectTrigger
            className="h-8 w-auto gap-2 text-sm"
            aria-label={`What tunnel ${connector.name} publishes`}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">
              every app under {connector.domain}
            </SelectItem>
            <SelectItem value="chosen">only hostnames you pick</SelectItem>
          </SelectContent>
        </Select>
      </div>
      {hostnamesQuery.isError ? (
        <p className="text-xs text-muted-foreground">
          {problemDetail(
            hostnamesQuery.error,
            'Could not load the hostnames under this tunnel.'
          )}
        </p>
      ) : hosts.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          No apps under {connector.domain} yet.
        </p>
      ) : (
        <ul className="divide-y">
          {hosts.map((view) => (
            <HostnameRow key={view.hostname} view={view} />
          ))}
        </ul>
      )}
    </div>
  )
}

/**
 * One hostname's tunnel status and choice, for the pages that list domains.
 * Renders nothing when no tunnel is involved.
 */
export function HostnameTunnelStatus({ hostname }: { hostname: string }) {
  const hostnamesQuery = useHostnames()
  const host = hostname.trim().replace(/\.$/, '').toLowerCase()
  const view = hostnamesQuery.data?.find((h) => h.hostname === host)
  if (
    !view ||
    (view.candidates.length === 0 &&
      view.mode === 'automatic' &&
      view.state === 'not_published')
  ) {
    return null
  }
  return (
    <div className="mt-2 space-y-1">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-muted-foreground">Tunnel</span>
        <StateBadge view={view} />
        <HostnameChoice view={view} />
      </div>
      <p className="text-xs text-muted-foreground">{view.detail}</p>
    </div>
  )
}
