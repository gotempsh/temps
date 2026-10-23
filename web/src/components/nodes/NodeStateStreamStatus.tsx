// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { Radio } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  getNodeStateStatus,
  nodeStateStatusQueryKey,
  nodeStateStreamSummary,
  type NodeStateTone,
} from '@/lib/node-workload'
import { refetchUnlessRefused } from '@/lib/query-retry'
import { isForbiddenRead } from '@/lib/read-failure'

const TONE_VARIANT: Record<
  NodeStateTone,
  'success-subtle' | 'warning-subtle' | 'neutral-subtle'
> = {
  ok: 'success-subtle',
  warning: 'warning-subtle',
  off: 'neutral-subtle',
}

/**
 * One line with the node state stream's status (ADR-055 §7): whether the
 * workload plane streams desired state to nodes, and how far it lags.
 */
export function NodeStateStreamStatus({
  context = 'Desired-state stream',
}: {
  context?: string
}) {
  const query = useQuery({
    queryKey: nodeStateStatusQueryKey,
    queryFn: getNodeStateStatus,
    refetchInterval: refetchUnlessRefused(15_000),
    meta: { errorTitle: 'Could not load the node state stream status' },
  })

  return (
    <div
      className="flex flex-col gap-2 rounded-lg border px-4 py-3 text-sm sm:flex-row sm:items-center"
      role="status"
      aria-live="polite"
    >
      <div className="flex shrink-0 items-center gap-2 font-medium">
        <Radio className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
        {context}
      </div>
      {query.isPending ? (
        <Skeleton className="h-5 w-48" />
      ) : query.isError ? (
        <span className="text-muted-foreground">
          {isForbiddenRead(query.error)
            ? 'You do not have permission to read the stream status.'
            : problemDetail(
                query.error,
                'The stream status could not be read.'
              )}
        </span>
      ) : (
        <StreamSummary status={query.data} />
      )}
    </div>
  )
}

function StreamSummary({
  status,
}: {
  status: Parameters<typeof nodeStateStreamSummary>[0]
}) {
  const summary = nodeStateStreamSummary(status)
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-2">
      <Badge variant={TONE_VARIANT[summary.tone]}>{summary.label}</Badge>
      <span className="min-w-0 text-muted-foreground">{summary.detail}</span>
    </div>
  )
}
