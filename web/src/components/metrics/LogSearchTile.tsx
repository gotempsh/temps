// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { savedSearchSeriesOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { ThresholdLineChart } from '@/components/charts/threshold-line-chart'
import { formatBucketLabel, formatMetricValue } from './metric-format'
import { logTileBucketSecs, logTileMetricLabel } from './log-tile'

interface LogSearchTileProps {
  projectId: number
  savedSearchId: number
  /** `count`, or `<fn>:<attribute>`. */
  metric: string
  /** Display title; falls back to the saved search's name. */
  title?: string | null
  /** Saved search names by id, to title a tile without one. */
  searchName?: string
  /** ISO bounds, memoized upstream so the query key stays stable. */
  fromIso: string
  toIso: string
  height?: number
}

/**
 * A dashboard tile charting a saved log search over time: matching lines
 * per bucket, or a numeric field. When the server counted by a capped scan,
 * the buckets it never reached have no value, and the tile says from when.
 */
export function LogSearchTile({
  projectId,
  savedSearchId,
  metric,
  title,
  searchName,
  fromIso,
  toIso,
  height = 160,
}: LogSearchTileProps) {
  const bucketSecs = logTileBucketSecs(fromIso, toIso, metric)
  const q = useQuery({
    ...savedSearchSeriesOptions({
      path: { project_id: projectId, saved_search_id: savedSearchId },
      query: {
        start_time: fromIso,
        end_time: toIso,
        bucket_secs: bucketSecs,
        metric,
      },
    }),
    enabled: projectId > 0 && savedSearchId > 0,
  })
  const data = useMemo(
    () =>
      (q.data?.points ?? []).map((point) => ({
        bucket: point.ts,
        label: formatBucketLabel(point.ts),
        value: point.value ?? null,
      })),
    [q.data?.points]
  )
  const latest = data.length ? data[data.length - 1].value : null
  const partialBefore = q.data?.partial_before
  const displayTitle = title?.trim() || searchName || 'Saved log search'
  const notFound =
    q.isError &&
    typeof q.error === 'object' &&
    q.error !== null &&
    'status' in q.error &&
    q.error.status === 404

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-card p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="truncate text-xs font-medium" title={displayTitle}>
          {displayTitle}
        </span>
        <Badge variant="secondary" className="shrink-0 text-[10px]">
          {logTileMetricLabel(metric)}
        </Badge>
      </div>

      {q.isPending ? (
        <Skeleton className="w-full" style={{ height }} />
      ) : q.isError ? (
        <div
          className="flex items-center justify-center text-center text-xs text-rose-700 dark:text-rose-500"
          style={{ height }}
        >
          {notFound
            ? 'This saved search was deleted. Edit the dashboard to pick another.'
            : 'Failed to load the log series'}
        </div>
      ) : metric !== 'count' && data.every((point) => point.value == null) ? (
        <div
          className="flex items-center justify-center text-center text-xs text-muted-foreground"
          style={{ height }}
        >
          No matching lines carry this field in range
        </div>
      ) : (
        <ThresholdLineChart
          data={data}
          xKey="label"
          series={{
            dataKey: 'value',
            label: logTileMetricLabel(metric),
            tone: 'primary',
          }}
          height={height}
          tooltipValueFormatter={(v) => formatMetricValue(v)}
          yTickFormatter={(v) => formatMetricValue(v)}
        />
      )}

      <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
        {partialBefore ? (
          <span className="text-[11px]">
            Counted from {formatBucketLabel(partialBefore)}; older buckets were
            not scanned
          </span>
        ) : (
          <span className="text-[10px] uppercase tracking-wide">latest</span>
        )}
        <span className="font-mono tabular-nums">
          {latest != null ? formatMetricValue(latest) : '—'}
        </span>
      </div>
    </div>
  )
}
