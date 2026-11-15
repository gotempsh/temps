// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// How visitors reach a worker, as a badge for the node list and header.

import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import { listIngressProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { ingressSetupUrl } from '@/components/nodes/ingress-setup'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import { workerIngressWay } from '@/lib/worker-ingress'

export function WorkerIngressBadge({
  node,
  linkSetup = true,
}: {
  node: NodeInfoResponse
  /** "No ingress" links to the setup page, except on that page itself. */
  linkSetup?: boolean
}) {
  // Shared with the ingress card's queries, so no extra request per row.
  const connectorsQuery = useQuery(allTunnelsOptions())
  const providersQuery = useQuery(listIngressProvidersOptions())
  if (node.role === 'control-plane' || !connectorsQuery.data) return null
  const way = workerIngressWay(node, connectorsQuery.data)
  if (way.kind === 'tunnel') {
    const names = way.providers.map(
      (kind) => providersQuery.data?.find((p) => p.kind === kind)?.name ?? kind
    )
    return (
      <Badge variant="outline" className="text-xs">
        {names.join(', ')} tunnel
      </Badge>
    )
  }
  if (way.kind === 'direct') {
    return (
      <Badge
        variant="outline"
        className={
          way.running
            ? 'text-xs'
            : 'border-amber-500/40 text-xs text-amber-700 dark:text-amber-400'
        }
        title={way.running ? undefined : 'Its listeners are not running'}
      >
        Open ports{way.running ? '' : ': not running'}
      </Badge>
    )
  }
  const badge = (
    <Badge
      variant="outline"
      className="border-amber-500/40 text-xs text-amber-700 dark:text-amber-400"
      title="Visitors can't reach apps through this worker yet"
    >
      No ingress
    </Badge>
  )
  if (!linkSetup || node.status !== 'active') return badge
  return (
    <Link
      to={ingressSetupUrl({ worker: node.id })}
      aria-label={`No ingress on ${node.name}: set it up`}
      className="rounded-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
      // Node rows open the node on click; this opens the setup instead.
      onClick={(event) => event.stopPropagation()}
    >
      {badge}
    </Link>
  )
}
