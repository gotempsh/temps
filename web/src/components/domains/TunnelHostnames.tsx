// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Which hostnames go through which tunnel, and why (ADR 058 D9). The
// control plane decides; these only show its reasoning and record the
// operator's choice.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ExternalLink } from 'lucide-react'
import { toast } from 'sonner'
import {
  getIngressHostnameOptions,
  retryIngressHostnameDnsMutation,
  listIngressConnectorsOptions,
  listIngressHostnamesOptions,
  setIngressHostnameChoiceMutation,
  updateIngressConnectorMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ConnectorView, HostnameView } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from '@/components/ui/dropdown-menu'
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

export function RetryHostnameDns({
  hostname,
  connectorId,
}: {
  hostname: string
  connectorId?: number | null
}) {
  const queries = useQueryClient()
  const refresh = useRefreshIngress()
  const detail = getIngressHostnameOptions({
    path: { hostname },
  })
  const retry = useMutation({
    ...retryIngressHostnameDnsMutation(),
    onSuccess: (result) => {
      queries.setQueryData(detail.queryKey, result)
      if (result.state === 'published') {
        toast.success(`DNS setup completed for ${result.hostname}`)
      } else {
        toast.error(`DNS setup failed for ${result.hostname}`, {
          description: result.detail,
        })
      }
    },
    onError: (error) =>
      toast.error(`Could not retry DNS setup for ${hostname}`, {
        description: problemDetail(
          error,
          'Refresh the hostname status and try again.'
        ),
      }),
    onSettled: () =>
      Promise.all([
        refresh(),
        queries.invalidateQueries({ queryKey: detail.queryKey }),
      ]),
  })
  if (connectorId == null && !retry.isPending) return null
  return (
    <Button
      variant="outline"
      size="sm"
      disabled={retry.isPending}
      onClick={(event) => {
        event.stopPropagation()
        if (connectorId != null)
          retry.mutate({ path: { id: connectorId, hostname } })
      }}
    >
      {retry.isPending ? 'Retrying DNS setup…' : 'Retry DNS setup'}
    </Button>
  )
}

export function StateBadge({ view }: { view: HostnameView }) {
  switch (view.state) {
    case 'published':
      return <Badge className="bg-green-600 hover:bg-green-600">Live</Badge>
    case 'pending':
      return <Badge variant="secondary">Publishing…</Badge>
    case 'error':
      return <Badge variant="destructive">Publication failed</Badge>
    default:
      return <Badge variant="outline">Not through a tunnel</Badge>
  }
}

/** The tunnel Automatic picks: the longest matching domain that publishes everything. */
function automaticCandidate(view: HostnameView) {
  return view.candidates
    .filter((c) => c.auto_publish)
    .sort(
      (a, b) =>
        b.domain.length - a.domain.length || a.connector_id - b.connector_id
    )[0]
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
  const value = choiceValue(view)
  const automatic = automaticCandidate(view)
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

function HostnameRow({
  view,
  connectorId,
}: {
  view: HostnameView
  connectorId: number
}) {
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
          <RetryHostnameDns
            hostname={view.hostname}
            connectorId={
              view.retry_dns_connector_id === connectorId ? connectorId : null
            }
          />
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
            <HostnameRow
              key={view.hostname}
              view={view}
              connectorId={connector.id}
            />
          ))}
        </ul>
      )}
    </div>
  )
}

/** The tunnel view of one hostname, or `undefined` while none is known. */
function useHostnameView(hostname: string): HostnameView | undefined {
  const hostnamesQuery = useHostnames()
  const host = hostname.trim().replace(/\.$/, '').toLowerCase()
  return hostnamesQuery.data?.find((h) => h.hostname === host)
}

/** Whether a tunnel has anything to do with a hostname. */
function tunnelInvolved(view: HostnameView | undefined): boolean {
  return (
    !!view &&
    !(
      view.candidates.length === 0 &&
      view.mode === 'automatic' &&
      view.state === 'not_published'
    )
  )
}

function choiceValue(view: HostnameView): string {
  return view.mode === 'tunnel' && view.chosen_connector_id != null
    ? `tunnel:${view.chosen_connector_id}`
    : view.mode
}

/**
 * The tunnel choice as a submenu of a hostname's actions menu: Automatic,
 * a specific tunnel, or no tunnel. Nothing when no tunnel is involved.
 */
export function HostnameTunnelMenu({ hostname }: { hostname: string }) {
  const view = useHostnameView(hostname)
  if (!view || !tunnelInvolved(view)) return null
  return <TunnelChoiceSubmenu view={view} />
}

function TunnelChoiceSubmenu({ view }: { view: HostnameView }) {
  const refresh = useRefreshIngress()
  const choose = useMutation({
    ...setIngressHostnameChoiceMutation(),
    onSuccess: async () => {
      await refresh()
      toast.success(`Changed how ${view.hostname} reaches visitors`)
    },
    onError: (error) =>
      toast.error(`Could not change ${view.hostname}`, {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })
  const automatic = automaticCandidate(view)
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger disabled={choose.isPending}>
        Tunnel
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent>
        <DropdownMenuRadioGroup
          value={choiceValue(view)}
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
          <DropdownMenuRadioItem value="automatic">
            Automatic{automatic ? ` (${automatic.name})` : ' (none)'}
          </DropdownMenuRadioItem>
          {view.candidates.map((c) => (
            <DropdownMenuRadioItem
              key={c.connector_id}
              value={`tunnel:${c.connector_id}`}
            >
              Through {c.name}
            </DropdownMenuRadioItem>
          ))}
          <DropdownMenuRadioItem value="off">
            Not through a tunnel
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  )
}

/**
 * A hostname's tunnel publication while it needs attention (publishing, or
 * failed, with the retry); nothing once it is live or no tunnel is involved.
 */
export function HostnameTunnelProblem({ hostname }: { hostname: string }) {
  const view = useHostnameView(hostname)
  if (!view) return null
  if (view.state !== 'pending' && view.state !== 'error') return null
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
      <StateBadge view={view} />
      <span>{view.detail}</span>
      <RetryHostnameDns
        hostname={view.hostname}
        connectorId={view.retry_dns_connector_id}
      />
    </div>
  )
}
