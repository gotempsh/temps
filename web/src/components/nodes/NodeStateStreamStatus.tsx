// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { HelpPopover } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { Info } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import {
  formatLag,
  getNodeStateStatus,
  NODE_STATE_LAG_WARNING_MS,
  nodeStateStatusQueryKey,
  nodeStateStreamSummary,
  type NodeStateStatus,
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

function Metric({
  name,
  value,
  children,
}: {
  name: string
  value: string
  children: string
}) {
  return (
    <div>
      <dt className="flex items-baseline justify-between gap-3 text-foreground">
        <span>{name}</span>
        <span className="font-medium tabular-nums">{value}</span>
      </dt>
      <dd className="text-xs">{children}</dd>
    </div>
  )
}

function StreamHelp({ status }: { status: NodeStateStatus | undefined }) {
  return (
    <HelpPopover label="About updates to nodes" icon={Info}>
      <p>
        Nodes learn what to run and which DNS names to answer from this stream,
        known as the desired-state stream. Without it, nodes pick up no changes.
      </p>
      {status?.configured && (
        <>
          <p>{nodeStateStreamSummary(status).detail}</p>
          <dl className="space-y-2">
            <Metric name="Delay" value={formatLag(status.watermark_lag_ms)}>
              {`How long changes wait on an older database write to finish. A warning shows from ${NODE_STATE_LAG_WARNING_MS / 1000} s.`}
            </Metric>
            <Metric name="Requests" value={status.reads.toLocaleString()}>
              Times nodes asked for changes since Temps last started.
            </Metric>
            <Metric
              name="Full refreshes"
              value={status.full_snapshots.toLocaleString()}
            >
              Times a node got everything rather than what changed: on first
              connecting, after a long time away, or after a very large change.
            </Metric>
          </dl>
        </>
      )}
    </HelpPopover>
  )
}

/**
 * One compact line with the node state stream's status (ADR-055 §7). A healthy
 * stream shows only its verdict, with what it means and its numbers a click
 * away; a problem says what is wrong right here.
 */
export function NodeStateStreamStatus() {
  const query = useQuery({
    queryKey: nodeStateStatusQueryKey,
    queryFn: getNodeStateStatus,
    refetchInterval: refetchUnlessRefused(15_000),
    meta: { errorTitle: 'Could not load the node state stream status' },
  })
  const summary = query.data ? nodeStateStreamSummary(query.data) : undefined

  return (
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
      <div
        className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1"
        role="status"
        aria-live="polite"
      >
        <span className="shrink-0 text-muted-foreground">Updates to nodes</span>
        {query.isPending ? (
          <Skeleton className="h-5 w-20" />
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
          summary && (
            <>
              <Badge variant={TONE_VARIANT[summary.tone]}>
                {summary.label}
              </Badge>
              {summary.tone !== 'ok' && (
                <span className="min-w-0 text-muted-foreground">
                  {summary.detail}
                </span>
              )}
            </>
          )
        )}
      </div>
      <StreamHelp status={query.data} />
    </div>
  )
}
