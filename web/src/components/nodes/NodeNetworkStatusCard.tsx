// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NodeInfoResponse } from '@/api/client'
import { clusterDnsStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { overlayRow, resolverRow, type NodeStatusTone } from '@/lib/node-detail'
import { CLUSTER_DNS_URL } from '@/lib/worker-nodes'
import { useQuery } from '@tanstack/react-query'
import { Network } from 'lucide-react'
import { Link } from 'react-router'

const BADGE: Record<
  NodeStatusTone,
  'success-subtle' | 'warning-subtle' | 'secondary'
> = {
  ok: 'success-subtle',
  warn: 'warning-subtle',
  muted: 'secondary',
}

/**
 * This node's place in the cluster network: its overlay subnet and its
 * cluster DNS resolver. Settings that change them are cluster-wide and live
 * on the Worker nodes page; this card links there.
 */
export function NodeNetworkStatusCard({ node }: { node: NodeInfoResponse }) {
  const dns = useQuery({ ...clusterDnsStatusOptions(), retry: false })
  const entry = dns.data?.nodes.find((n) => n.node_id === node.id)
  const rows = [
    {
      label: 'Private address',
      value: node.private_address,
      tone: 'muted' as NodeStatusTone,
    },
    overlayRow(node),
    resolverRow(dns.data?.cluster_dns_enabled, entry),
  ]
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Network className="h-4 w-4" />
          Cluster network
        </CardTitle>
        <CardDescription>
          How other nodes reach this one, and how its applications find services
          by name.{' '}
          <Link to={CLUSTER_DNS_URL} className="underline underline-offset-2">
            Cluster-wide settings
          </Link>
        </CardDescription>
      </CardHeader>
      <CardContent>
        <dl className="divide-y rounded-lg border">
          {rows.map((row) => (
            <div
              key={row.label}
              className="flex flex-col gap-1 p-3 sm:flex-row sm:items-start sm:justify-between"
            >
              <dt className="text-sm text-muted-foreground">{row.label}</dt>
              <dd className="space-y-1 sm:text-right">
                {row.tone === 'muted' && row.label === 'Private address' ? (
                  <span className="font-mono text-sm">{row.value}</span>
                ) : (
                  <Badge variant={BADGE[row.tone]}>{row.value}</Badge>
                )}
                {row.detail && (
                  <p className="max-w-prose text-xs text-muted-foreground">
                    {row.detail}
                  </p>
                )}
              </dd>
            </div>
          ))}
        </dl>
      </CardContent>
    </Card>
  )
}
