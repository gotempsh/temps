// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// How visitors reach a worker, as it stands: read-only. Changing it is
// behind the card's Change button (WorkerIngressCard).

import { useQuery } from '@tanstack/react-query'
import { CheckCircle2, ExternalLink, XCircle } from 'lucide-react'
import type { ReactNode } from 'react'
import {
  listIngressHostnamesOptions,
  listIngressProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import type {
  ConnectorView,
  NodeInfoResponse,
  PublicIngressSettings,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { TunnelLinks } from '@/components/nodes/TunnelLinks'
import { replicaState } from '@/components/nodes/WorkerTunnels'
import { Link } from 'react-router'
import { ingressSetupUrl } from '@/components/nodes/ingress-setup'
import { workerIngressStatus } from '@/lib/worker-ingress'

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-1 sm:grid-cols-[10rem_1fr] sm:gap-4">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="min-w-0 space-y-1 text-sm">{children}</dd>
    </div>
  )
}

function Status({ ok, children }: { ok: boolean; children: ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      {ok ? (
        <CheckCircle2 className="h-4 w-4 shrink-0 text-green-600" />
      ) : (
        <XCircle className="h-4 w-4 shrink-0 text-destructive" />
      )}
      {children}
    </span>
  )
}

function TunnelRow({
  connector,
  nodeId,
  providerName,
}: {
  connector: ConnectorView
  nodeId: number
  providerName: string
}) {
  const hostnamesQuery = useQuery({
    ...listIngressHostnamesOptions(),
    refetchInterval: 15_000,
    retry: false,
  })
  const replica = connector.nodes.find((n) => n.node_id === nodeId)
  const hosts = (hostnamesQuery.data ?? []).filter(
    (h) => h.connector_id === connector.id
  )
  const live = hosts.filter((h) => h.state === 'published')
  const failing = hosts.filter((h) => h.state === 'error').length
  const others = connector.nodes.length - 1

  let status: { ok: boolean; label: string }
  if (connector.status === 'error') {
    status = {
      ok: false,
      label: connector.last_error ?? 'The provider refused the tunnel.',
    }
  } else if (connector.status === 'pending' || !replica) {
    status = { ok: false, label: 'Being created' }
  } else {
    const replicaStatus = replicaState(replica)
    status = {
      ok: replicaStatus.ok,
      label: replica.last_error
        ? `${replicaStatus.label}: ${replica.last_error}`
        : replicaStatus.label,
    }
  }

  return (
    <Row label="Tunnel">
      <p>
        <span className="font-medium">{connector.name}</span>{' '}
        <span className="text-muted-foreground">
          · {providerName} · apps under {connector.domain}
          {others > 0 &&
            ` · also on ${others} other worker${others > 1 ? 's' : ''}`}
        </span>
      </p>
      <Status ok={status.ok}>{status.label}</Status>
      {connector.status === 'ready' && replica?.state === 'ready' && (
        <TunnelLinks
          connector={connector}
          nodeId={nodeId}
          readyConnections={replica.ready_connections}
        />
      )}
      {live.length > 0 ? (
        <p className="flex flex-wrap gap-x-3 gap-y-0.5 text-xs">
          {live.map((h) => (
            <a
              key={h.hostname}
              className="inline-flex items-center gap-1 hover:underline"
              href={`https://${h.hostname}`}
              target="_blank"
              rel="noreferrer"
            >
              {h.hostname} <ExternalLink className="h-3 w-3" />
            </a>
          ))}
        </p>
      ) : (
        <p className="text-xs text-muted-foreground">No apps published yet.</p>
      )}
      {failing > 0 && (
        <p className="text-xs text-destructive">
          {failing} hostname{failing > 1 ? 's' : ''} could not be published.
          Change shows why.
        </p>
      )}
    </Row>
  )
}

export function WorkerIngressSummary({
  node,
  settings,
  onChange,
}: {
  node: NodeInfoResponse
  settings: PublicIngressSettings
  onChange: () => void
}) {
  const connectorsQuery = useQuery({
    ...allTunnelsOptions(),
    refetchInterval: 10_000,
  })
  const providersQuery = useQuery(listIngressProvidersOptions())
  const here = (connectorsQuery.data ?? []).filter((c) =>
    c.nodes.some((n) => n.node_id === node.id)
  )
  const providerName = (kind: string) =>
    providersQuery.data?.find((p) => p.kind === kind)?.name ?? kind
  const state = workerIngressStatus(node)
  const workerOff = settings.mode === 'off'
  const listen =
    settings.listening_http ??
    `${settings.listen_address ?? 'default address'}:${settings.http_port}`

  if (!workerOff && here.length === 0 && !settings.enabled) {
    return (
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-dashed p-3">
        <p className="text-sm text-muted-foreground">
          Visitors can&apos;t reach apps through this worker yet.
        </p>
        <div className="flex flex-wrap gap-2">
          <Button asChild size="sm">
            <Link to={ingressSetupUrl({ worker: node.id })}>
              Set up ingress
            </Link>
          </Button>
          <Button size="sm" variant="outline" onClick={onChange}>
            Configure here
          </Button>
        </div>
      </div>
    )
  }

  return (
    <dl className="space-y-3">
      {here.map((connector) => (
        <TunnelRow
          key={connector.id}
          connector={connector}
          nodeId={node.id}
          providerName={providerName(connector.provider)}
        />
      ))}
      <Row label="Open ports">
        {workerOff ? (
          <p className="text-muted-foreground">
            Off. The worker doesn&apos;t take public traffic.
          </p>
        ) : !settings.enabled ? (
          <p className="text-muted-foreground">Off</p>
        ) : (
          <>
            <p>
              {listen}
              {settings.listening_https && ` and ${settings.listening_https}`}
              {settings.effective_public_address && (
                <span className="text-muted-foreground">
                  {' '}
                  · public {settings.effective_public_address}
                </span>
              )}
            </p>
            <Status ok={state.label === 'Listeners running'}>
              {state.label === 'Listeners running'
                ? state.label
                : `${state.label}: ${state.description}`}
            </Status>
          </>
        )}
      </Row>
    </dl>
  )
}
