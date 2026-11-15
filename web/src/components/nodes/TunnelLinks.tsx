// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The links a worker's tunnel connector holds to the provider's edge: a
// one-line summary, and each link when opened. Read from the provider on
// request, so only shown while the worker reports the connector running.

import { useQuery } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { ChevronRight } from 'lucide-react'
import { useState } from 'react'
import { getIngressConnectorLinksOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ConnectorView, LinkView } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { problemDetail } from '@/lib/api-problem'
import { cn } from '@/lib/utils'

/** `mad01` → `MAD`: the airport code the provider names its data centres by. */
function place(location?: string | null): string {
  return (location ?? '').replace(/\d+$/, '').toUpperCase() || 'Unknown'
}

function plural(n: number, word: string) {
  return `${n} ${word}${n === 1 ? '' : 's'}`
}

function LinkRow({ link }: { link: LinkView }) {
  return (
    <li className="flex flex-wrap items-center gap-x-2 text-xs">
      <span className="w-12 font-medium">{place(link.location)}</span>
      <span className="text-muted-foreground">
        {link.location}
        {link.opened_at &&
          ` · open ${formatDistanceToNow(new Date(link.opened_at), { addSuffix: false })}`}
      </span>
      {link.reconnecting && <Badge variant="secondary">Reconnecting</Badge>}
    </li>
  )
}

export function TunnelLinks({
  connector,
  nodeId,
  readyConnections,
}: {
  connector: ConnectorView
  nodeId: number
  /** What the worker last reported, shown until the provider answers. */
  readyConnections?: number | null
}) {
  const [open, setOpen] = useState(false)
  const linksQuery = useQuery({
    ...getIngressConnectorLinksOptions({ path: { id: connector.id } }),
    refetchInterval: 30_000,
    retry: false,
  })
  const all = linksQuery.data?.links ?? []
  const matched = all.filter((l) => l.node_id === nodeId)
  // Before the worker reports which connector it runs, nothing matches; the
  // unmatched links are then the best guess at this worker's.
  const mine = matched.length > 0 ? matched : all.filter((l) => l.node_id == null)
  const elsewhere = all.filter((l) => l.node_id != null && l.node_id !== nodeId)
  const count = linksQuery.data ? mine.length : readyConnections
  if (count == null && !linksQuery.isError) return null

  const places = [...new Set(mine.map((l) => place(l.location)))]
  const reconnecting = mine.filter((l) => l.reconnecting).length
  const version = mine.find((l) => l.client_version)?.client_version

  return (
    <Collapsible open={open} onOpenChange={setOpen} className="ml-6">
      <CollapsibleTrigger className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
        <ChevronRight
          className={cn('h-3 w-3 transition-transform', open && 'rotate-90')}
        />
        {count != null ? plural(count, 'link') : 'Links'} to the provider
        {places.length > 0 && ` via ${places.join(', ')}`}
        {reconnecting > 0 && ` · ${reconnecting} reconnecting`}
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-1 space-y-1 pl-4">
        {linksQuery.isError ? (
          <p className="text-xs text-muted-foreground">
            {problemDetail(
              linksQuery.error,
              'Could not ask the provider about this tunnel.'
            )}
          </p>
        ) : mine.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            {linksQuery.isPending
              ? 'Asking the provider…'
              : 'The provider sees no links from this worker.'}
          </p>
        ) : (
          <>
            <ul className="space-y-0.5">
              {mine.map((link, i) => (
                <LinkRow key={i} link={link} />
              ))}
            </ul>
            <p className="text-xs text-muted-foreground">
              Each link is an outbound connection from this worker; the
              provider spreads them over data centres so one can drop without
              taking the apps down.
              {version && ` Connector version ${version}.`}
              {elsewhere.length > 0 &&
                ` ${plural(elsewhere.length, 'more link')} from other workers.`}
            </p>
          </>
        )}
      </CollapsibleContent>
    </Collapsible>
  )
}
