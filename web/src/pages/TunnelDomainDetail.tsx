// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A domain a tunnel carries (ADR 058 D10), opened from the Domains list.
// `*.zone` is the tunnel itself: where it runs and what it publishes. An
// exact name is one hostname: whether it is live, why not, and its route.

import { useEffect, type ReactNode } from 'react'
import { Link, useParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft, ExternalLink } from 'lucide-react'
import {
  Callout,
  Detail,
  Status,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { getIngressHostnameOptions } from '@/api/client/@tanstack/react-query.gen'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import type { ConnectorView, HostnameView } from '@/api/client/types.gen'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  HostnameChoice,
  TunnelHostnames,
} from '@/components/domains/TunnelHostnames'
import { TunnelLinks } from '@/components/nodes/TunnelLinks'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'

const HTTPS_NOTE =
  'Cloudflare serves HTTPS and renews the certificate. Its free certificate ' +
  'covers the zone and one level below it.'

function connectorVerdict(connector: ConnectorView): {
  tone: StatusTone
  label: string
} {
  switch (connector.status) {
    case 'ready':
    case 'active':
      return { tone: 'ok', label: 'Connected' }
    case 'error':
      return { tone: 'error', label: 'Failed' }
    case 'pending':
      return { tone: 'running', label: 'Setting up' }
    default:
      return { tone: 'idle', label: connector.status }
  }
}

function hostnameVerdict(view: HostnameView): {
  tone: StatusTone
  label: string
} {
  switch (view.state) {
    case 'published':
      return { tone: 'ok', label: 'Live' }
    case 'pending':
      return { tone: 'running', label: 'Publishing…' }
    case 'error':
      return { tone: 'error', label: 'Failed' }
    default:
      return { tone: 'idle', label: 'Not through a tunnel' }
  }
}

function ZoneDetail({
  hostname,
  connector,
  back,
}: {
  hostname: string
  connector: ConnectorView
  back: ReactNode
}) {
  const verdict = connectorVerdict(connector)
  const connected = connector.nodes.filter((n) => n.state === 'ready').length
  const facts: DetailFact[] = [
    { label: 'Tunnel', value: connector.name },
    {
      label: 'Workers connected',
      value: `${connected} of ${connector.nodes.length}`,
    },
    { label: 'HTTPS', value: 'Cloudflare' },
  ]
  if (connector.dns_target) {
    facts.push({
      label: 'DNS records point at',
      value: <code className="text-xs">{connector.dns_target}</code>,
    })
  }
  return (
    <Detail
      title={<span className="break-all">{hostname}</span>}
      description={`Apps under ${connector.domain} reach visitors through this tunnel. Temps writes each app's DNS record; workers need no public address or open port. ${HTTPS_NOTE}`}
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={back}
      facts={facts}
      main={
        <>
          {connector.status === 'error' && (
            <Callout tone="error" title="The tunnel is not working">
              {connector.last_error ?? 'Setting it up failed.'}
            </Callout>
          )}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Runs on</CardTitle>
              <CardDescription>
                The workers that keep this tunnel connected.
              </CardDescription>
            </CardHeader>
            <CardContent>
              {connector.nodes.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No worker. Pick one from a worker&apos;s page.
                </p>
              ) : (
                <ul className="space-y-3 text-sm">
                  {connector.nodes.map((node) => (
                    <li key={node.node_id} className="space-y-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Link
                          to={`/settings/nodes/${node.node_id}`}
                          className="hover:underline"
                        >
                          {node.node_name}
                        </Link>
                        <Badge
                          variant={
                            node.state === 'ready'
                              ? 'default'
                              : node.state === 'error'
                                ? 'destructive'
                                : 'secondary'
                          }
                          className="text-xs"
                        >
                          {node.state === 'ready' ? 'connected' : node.state}
                        </Badge>
                      </div>
                      {node.last_error && node.state !== 'ready' && (
                        <p className="text-xs text-muted-foreground">
                          {node.last_error}
                        </p>
                      )}
                      {node.state === 'ready' && (
                        <TunnelLinks
                          connector={connector}
                          nodeId={node.node_id}
                          readyConnections={node.ready_connections}
                        />
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Hostnames</CardTitle>
              <CardDescription>
                What it publishes, and how each name reaches visitors.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <TunnelHostnames connector={connector} />
            </CardContent>
          </Card>
        </>
      }
    />
  )
}

function HostnameDetail({
  view,
  back,
}: {
  view: HostnameView
  back: ReactNode
}) {
  const verdict = hostnameVerdict(view)
  const facts: DetailFact[] = [
    {
      label: 'Tunnel',
      value: view.connector_name ?? (
        <span className="text-muted-foreground">None</span>
      ),
    },
    {
      label: 'Served by an app',
      value: view.served_by_workers ? 'Yes, on a worker' : 'Not yet',
    },
    { label: 'HTTPS', value: 'Cloudflare' },
  ]
  if (view.published_at) {
    facts.push({
      label: 'Published',
      value: new Date(view.published_at).toLocaleString(),
    })
  }
  return (
    <Detail
      title={<span className="break-all">{view.hostname}</span>}
      description={view.detail}
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={
        <>
          {view.state === 'published' && (
            <Button variant="outline" asChild>
              <a
                href={`https://${view.hostname}`}
                target="_blank"
                rel="noreferrer"
              >
                <ExternalLink className="mr-2 size-4" />
                Open
              </a>
            </Button>
          )}
          {back}
        </>
      }
      facts={facts}
      main={
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Reaches visitors</CardTitle>
            <CardDescription>
              {view.served_by_workers
                ? HTTPS_NOTE
                : "No app serves this name on a worker yet. Add it to an environment's domains and deploy on a worker."}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <HostnameChoice view={view} />
          </CardContent>
        </Card>
      }
    />
  )
}

export function TunnelDomainDetail() {
  const { hostname = '' } = useParams<{ hostname: string }>()
  const zone = hostname.startsWith('*.') ? hostname.slice(2) : undefined
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Domains', href: '/domains' },
      { label: hostname },
    ])
  }, [setBreadcrumbs, hostname])
  usePageTitle(`${hostname} - Domain Details`)

  const connectorsQuery = useQuery({
    ...allTunnelsOptions(),
    enabled: !!zone,
    refetchInterval: 15_000,
    retry: false,
  })
  const hostnameQuery = useQuery({
    ...getIngressHostnameOptions({ path: { hostname } }),
    enabled: !zone && !!hostname,
    refetchInterval: 15_000,
    retry: false,
  })
  const query = zone ? connectorsQuery : hostnameQuery
  const connector = zone
    ? connectorsQuery.data?.find((c) => c.domain === zone)
    : undefined

  const back = (
    <Button variant="outline" asChild>
      <Link to="/domains">
        <ArrowLeft className="mr-2 size-4" />
        Back to Domains
      </Link>
    </Button>
  )
  const description = zone
    ? 'Every name under this zone, through a tunnel'
    : 'Through a tunnel'
  const title = <span className="break-all">{hostname}</span>

  if (query.isLoading) {
    return (
      <PageContainer>
        <PageHeader title={title} description={description} />
        <Skeleton className="h-48 w-full" />
      </PageContainer>
    )
  }
  if (query.isError) {
    return (
      <PageContainer>
        <PageHeader title={title} description={description} actions={back} />
        <Callout tone="error" title={`Could not load ${hostname}`}>
          {problemDetail(query.error, 'Try again in a moment.')}{' '}
          <Button variant="link" onClick={() => query.refetch()}>
            Retry
          </Button>
        </Callout>
      </PageContainer>
    )
  }
  if (zone && connector) {
    return <ZoneDetail hostname={hostname} connector={connector} back={back} />
  }
  if (!zone && hostnameQuery.data) {
    return <HostnameDetail view={hostnameQuery.data} back={back} />
  }
  return (
    <PageContainer>
      <PageHeader
        title={zone ? `No tunnel covers ${zone}` : title}
        description="It may have been deleted."
        actions={back}
      />
    </PageContainer>
  )
}
