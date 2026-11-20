// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getOperationTrendOptions } from '@/api/client/@tanstack/react-query.gen'
import { ThresholdLineChart } from '@/components/charts/threshold-line-chart'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import {
  deploymentMarkers,
  hourTick,
  trendChartRows,
} from '@/lib/operation-trend'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { OperationBreakdown } from './OperationBreakdown'
import { Link } from 'react-router'

export interface TrendTarget {
  serviceName: string
  spanName: string
}

interface OperationTrendSheetProps {
  projectId: number
  projectSlug: string
  target: TrendTarget | null
  onClose: () => void
}

function formatMs(ms: number): string {
  if (!Number.isFinite(ms)) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  return `${ms.toFixed(ms >= 10 ? 0 : 1)}ms`
}

/**
 * Two weeks of one operation's hourly p95 and p50, with the deployments that
 * went live in that time drawn as markers.
 */
export function OperationTrendSheet({
  projectId,
  projectSlug,
  target,
  onClose,
}: OperationTrendSheetProps) {
  const { data, isLoading, isError, error } = useQuery({
    ...getOperationTrendOptions({
      query: {
        project_id: projectId,
        service_name: target?.serviceName ?? '',
        span_name: target?.spanName ?? '',
      },
    }),
    enabled: target !== null,
  })

  const rows = useMemo(() => trendChartRows(data?.points ?? []), [data])
  const markers = useMemo(
    () => deploymentMarkers(rows, data?.deployments ?? []),
    [rows, data]
  )
  const tracking = data?.tracking

  return (
    <Sheet open={target !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full sm:max-w-2xl overflow-y-auto">
        <SheetHeader>
          <SheetTitle className="break-all">{target?.spanName}</SheetTitle>
          <SheetDescription>
            {target?.serviceName} · hourly latency over the last 14 days, with
            deployments marked, and where the time of its calls goes
          </SheetDescription>
        </SheetHeader>
        <div className="space-y-4 px-4 pb-6">
          {isLoading ? (
            <Skeleton className="h-[280px] w-full" />
          ) : isError ? (
            <Alert variant="destructive">
              <AlertDescription>
                {error instanceof Error
                  ? error.message
                  : 'The trend could not be loaded.'}
              </AlertDescription>
            </Alert>
          ) : tracking && !tracking.configured ? (
            <Alert>
              <AlertDescription>
                {tracking.reason} Operations come from the traces this project
                sends;{' '}
                <Link
                  className="underline"
                  to={`/projects/${projectSlug}/${tracking.setup_path ?? 'traces'}`}
                >
                  see traces
                </Link>
                .
              </AlertDescription>
            </Alert>
          ) : (
            <>
              <ThresholdLineChart
                data={rows}
                xKey="hour"
                series={[
                  { dataKey: 'p95', label: 'p95', tone: 'primary' },
                  { dataKey: 'p50', label: 'p50', tone: 'neutral' },
                ]}
                markers={markers}
                height={280}
                xTickFormatter={hourTick}
                yTickFormatter={(v) => formatMs(v)}
                tooltipValueFormatter={formatMs}
                emptyMessage="No calls of this operation were rolled up in the last 14 days. History starts when the operation is called and is kept per complete hour."
              />
              {tracking && tracking.overflow_operations > 0 && (
                <p className="text-xs text-muted-foreground">
                  The last hour had {tracking.overflow_operations} more
                  operations than the {tracking.max_tracked_operations} kept per
                  hour; operations with the least total time have no history for
                  that hour.
                </p>
              )}
              {tracking?.last_error && (
                <p className="text-xs text-destructive">
                  The last rollup failed: {tracking.last_error}
                </p>
              )}
            </>
          )}
          {target && (
            <OperationBreakdown
              key={`${target.serviceName}-${target.spanName}`}
              projectId={projectId}
              serviceName={target.serviceName}
              spanName={target.spanName}
            />
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}
