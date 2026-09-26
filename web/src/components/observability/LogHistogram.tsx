// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Line-count histogram above the global log list (ADR-047 §5).
 *
 * Needs the line index (ClickHouse, Temps Cloud or the TimescaleDB
 * fallback): when none is available this still
 * renders — the onboarding state names exactly what's missing, shows the
 * example, and links to Settings → Metrics & monitoring (CLAUDE.md:
 * unconfigured features onboard instead of disappearing). `text` search has
 * no analog in the index (it holds no message bytes), so an active text
 * filter is dropped for this chart only, and that is said out loud rather
 * than silently changing what the numbers mean.
 */

import { useMemo, useState } from 'react'
import {
  Bar,
  BarChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'
import { Link } from 'react-router'
import { ChevronDown } from 'lucide-react'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { BREAKDOWN_STROKES } from '@/components/charts/chart-colors'
import { TOOLTIP_CONTENT_STYLE, TOOLTIP_LABEL_STYLE } from '@/lib/chart-tooltip'
import { cn } from '@/lib/utils'
import type { AnalyticsCapability } from '@/api/client/types.gen'
import {
  histogramBucketSeconds,
  useGlobalLogAttributeKeys,
  useGlobalLogHistogram,
  type GlobalLogFilters,
  type HistogramGroupBy,
} from '@/hooks/useGlobalLogs'

const OTHER_GROUP = '__other__'
/** Sentinel for the "None" group-by option — Select item values can't be ''. */
const NONE_GROUP = 'none'

/** Always-visible reason + example + setup link; never a feature that vanishes. */
function AnalyticsOnboarding({
  analytics,
  compact,
}: {
  analytics: AnalyticsCapability
  compact?: boolean
}) {
  return (
    <div
      className={
        compact
          ? 'space-y-1.5 text-xs'
          : 'flex h-[220px] flex-col items-center justify-center gap-2 px-6 text-center'
      }
    >
      <p
        className={
          compact ? 'font-medium text-foreground' : 'text-sm font-medium'
        }
      >
        {analytics.reason ?? 'Attribute analytics are not configured.'}
      </p>
      <p className="text-xs text-muted-foreground">{analytics.example}</p>
      <Link
        to={analytics.setup_path}
        className="text-xs underline underline-offset-2 hover:text-foreground"
      >
        Configure in Settings
      </Link>
    </div>
  )
}

export function LogHistogram({
  filters,
  capability,
  capabilityLoading,
  onRangeSelect,
}: {
  filters: GlobalLogFilters
  capability?: AnalyticsCapability
  capabilityLoading: boolean
  onRangeSelect: (from: string, to: string) => void
}) {
  const [uiGroupBy, setUiGroupBy] = useState<string>(NONE_GROUP)
  const [expandedOverride, setExpandedOverride] = useState<boolean | null>(null)
  const groupBy: HistogramGroupBy =
    uiGroupBy === NONE_GROUP ? '' : (uiGroupBy as HistogramGroupBy)
  const configured = capability?.configured === true
  const bucketSecs = useMemo(
    () => histogramBucketSeconds(filters.start_time, filters.end_time),
    [filters.start_time, filters.end_time]
  )
  // The index holds no message bytes, so a text filter cannot be honored here.
  // Ask for the same filters minus `text` and say so, rather than pretend.
  const scopedFilters = useMemo(
    () => ({ ...filters, text: undefined }),
    [filters]
  )
  const attributeKeys = useGlobalLogAttributeKeys(scopedFilters, configured)
    .data?.keys
  const histogram = useGlobalLogHistogram(
    scopedFilters,
    bucketSecs,
    groupBy,
    configured
  )

  const { rows, groups } = useMemo(() => {
    const buckets = histogram.data?.buckets ?? []
    const rowsByTs = new Map<number, Record<string, number | string>>()
    const groupSet = new Set<string>()
    for (const bucket of buckets) {
      const ts = Date.parse(bucket.ts)
      if (!Number.isFinite(ts)) continue
      // Ungrouped responses carry no `group` at all — key every bucket under
      // the same 'count' series the single <Bar> below reads, rather than the
      // group-labelled key only the grouped branch ever renders.
      const group = groupBy ? bucket.group || OTHER_GROUP : 'count'
      if (groupBy) groupSet.add(group)
      const row = rowsByTs.get(ts) ?? { ts }
      row[group] = (Number(row[group]) || 0) + bucket.count
      rowsByTs.set(ts, row)
    }
    return {
      rows: [...rowsByTs.values()].sort((a, b) => Number(a.ts) - Number(b.ts)),
      groups: [...groupSet].sort(),
    }
  }, [histogram.data, groupBy])

  const totalLines = rows.reduce(
    (sum, row) =>
      sum +
      Object.entries(row)
        .filter(([key]) => key !== 'ts')
        .reduce((s, [, v]) => s + Number(v), 0),
    0
  )
  const showDetails = expandedOverride ?? (!capabilityLoading && !configured)
  const bucketLabel =
    bucketSecs < 60 ? `${bucketSecs}s` : `${Math.round(bucketSecs / 60)}m`
  const summary = capabilityLoading
    ? 'Checking log volume…'
    : !configured
      ? (capability?.reason ?? 'Line index not configured')
      : histogram.isPending
        ? 'Loading log volume…'
        : histogram.isError
          ? 'Log volume unavailable'
          : `${totalLines.toLocaleString()} lines · ${bucketLabel} buckets${filters.text ? ' · text filter excluded' : ''}`

  return (
    <Card>
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-3 p-3">
        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
          <button
            type="button"
            className="inline-flex items-center gap-2 rounded px-1 py-1 text-sm font-semibold hover:bg-muted/50"
            onClick={() => setExpandedOverride(!showDetails)}
            aria-expanded={showDetails}
            aria-controls="global-log-volume-details"
          >
            <ChevronDown
              className={cn(
                'size-4 text-muted-foreground transition-transform',
                showDetails && 'rotate-180'
              )}
            />
            Log volume
          </button>
          <span className="text-xs text-muted-foreground">{summary}</span>
        </div>
        {configured && (
          <div hidden={!showDetails}>
            <Select value={uiGroupBy} onValueChange={setUiGroupBy}>
              <SelectTrigger className="h-8 w-[160px] text-xs">
                <SelectValue placeholder="Group by" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={NONE_GROUP}>None</SelectItem>
                <SelectItem value="level">Level</SelectItem>
                <SelectItem value="service">Service</SelectItem>
                {(attributeKeys ?? []).map((key) => (
                  <SelectItem key={key.value} value={`attr:${key.value}`}>
                    {key.value}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
      </CardHeader>
      <CardContent
        id="global-log-volume-details"
        hidden={!showDetails}
        className="border-t p-4"
      >
        {configured && (
          <p className="mb-2 text-xs text-muted-foreground">
            Click a bar to zoom in.
          </p>
        )}
        {filters.text && (
          <p className="mb-2 text-[11px] text-muted-foreground">
            Excludes the text filter — the line index has no message bytes to
            match against. Showing counts for the other filters only.
          </p>
        )}
        {capabilityLoading ? (
          <Skeleton className="h-[180px] w-full" />
        ) : !configured ? (
          <AnalyticsOnboarding
            analytics={
              capability ?? {
                configured: false,
                reason:
                  'Attribute analytics are not configured on this instance.',
                example:
                  'Group ERROR lines by http_route for the last hour, chart requests slower than 500 ms per service, or facet on any field your app logs.',
                setup_path: '/settings/metrics-monitoring',
                live_chunks: 0,
                indexed_chunks: 0,
                forget_backlog: 0,
              }
            }
          />
        ) : histogram.isPending ? (
          <Skeleton className="h-[180px] w-full" />
        ) : histogram.isError ? (
          <div className="flex h-[180px] items-center justify-center text-sm text-muted-foreground">
            Histogram could not be loaded.
          </div>
        ) : rows.length === 0 ? (
          <div className="flex h-[180px] items-center justify-center text-sm text-muted-foreground">
            No lines in this window.
          </div>
        ) : (
          <div className="h-[180px]">
            <ResponsiveContainer width="100%" height="100%">
              <BarChart
                data={rows}
                margin={{ top: 4, right: 12, left: 0, bottom: 0 }}
              >
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="rgba(128,128,128,0.15)"
                  vertical={false}
                />
                <XAxis
                  dataKey="ts"
                  type="number"
                  domain={['dataMin', 'dataMax']}
                  tick={{ fontSize: 10, fill: 'rgba(156,163,175,0.9)' }}
                  tickLine={false}
                  axisLine={false}
                  tickFormatter={(ts) =>
                    new Date(ts).toLocaleTimeString(undefined, {
                      hour: '2-digit',
                      minute: '2-digit',
                      timeZone: 'UTC',
                    })
                  }
                />
                <YAxis
                  tick={{ fontSize: 10, fill: 'rgba(156,163,175,0.9)' }}
                  tickLine={false}
                  axisLine={false}
                  width={44}
                  allowDecimals={false}
                />
                <Tooltip
                  wrapperStyle={{ zIndex: 50 }}
                  contentStyle={TOOLTIP_CONTENT_STYLE}
                  labelStyle={TOOLTIP_LABEL_STYLE}
                  cursor={{ fill: 'rgba(128,128,128,0.1)' }}
                  labelFormatter={(ts) =>
                    `${new Date(Number(ts)).toLocaleString(undefined, { timeZone: 'UTC' })} UTC`
                  }
                  formatter={(value, name) => [
                    value,
                    name === OTHER_GROUP ? 'other' : String(name),
                  ]}
                />
                {(groupBy ? groups : ['count']).map((group, index) => (
                  <Bar
                    key={group}
                    dataKey={groupBy ? group : 'count'}
                    name={group}
                    stackId="lines"
                    fill={BREAKDOWN_STROKES[index % BREAKDOWN_STROKES.length]}
                    maxBarSize={18}
                    isAnimationActive={false}
                    onClick={(entry) => {
                      const ts = Number(
                        (entry as { payload?: { ts?: number } })?.payload?.ts
                      )
                      if (!Number.isFinite(ts)) return
                      onRangeSelect(
                        new Date(ts).toISOString(),
                        new Date(ts + bucketSecs * 1000).toISOString()
                      )
                    }}
                    cursor="pointer"
                  />
                ))}
              </BarChart>
            </ResponsiveContainer>
          </div>
        )}
        {configured &&
          rows.length > 0 &&
          capability &&
          capability.live_chunks > 0 && (
            <p className="mt-2 text-[11px] text-muted-foreground">
              Index coverage {capability.indexed_chunks.toLocaleString()}/
              {capability.live_chunks.toLocaleString()} chunks
            </p>
          )}
      </CardContent>
    </Card>
  )
}
