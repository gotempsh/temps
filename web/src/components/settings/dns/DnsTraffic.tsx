// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery, keepPreviousData } from '@tanstack/react-query'
import { TimeChart, type TimeChartSeries } from '@temps-sdk/ds'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Skeleton,
} from '@temps-sdk/ui'
import { seriesLineColor } from '@/components/charts/chart-colors'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  DNS_TRAFFIC_RANGES,
  dnsKeys,
  formatPercent,
  getDnsTraffic,
  trafficPoints,
  trafficTotals,
  type DnsResolverEntry,
  type DnsTrafficRange,
} from '@/lib/dns-observability'
import { isForbiddenRead } from '@/lib/read-failure'

const RANGE_LABEL: Record<DnsTrafficRange, string> = {
  '1h': 'Last hour',
  '6h': 'Last 6 hours',
  '24h': 'Last 24 hours',
  '7d': 'Last 7 days',
}

const QUERY_SERIES: TimeChartSeries[] = [
  { dataKey: 'queries', label: 'Queries', tone: 'neutral' },
  { dataKey: 'nxdomain', label: 'NXDOMAIN', tone: 'warn' },
  { dataKey: 'servfail', label: 'SERVFAIL and errors', tone: 'poor' },
]

function timeTick(range: DnsTrafficRange) {
  return (value: string | number) => {
    const date = new Date(value)
    return range === '7d'
      ? date.toLocaleDateString([], { month: 'short', day: 'numeric' })
      : date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  }
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="text-lg font-semibold tabular-nums">{value}</p>
    </div>
  )
}

function Legend({ series }: { series: TimeChartSeries[] }) {
  return (
    <ul className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
      {series.map((entry, index) => (
        <li key={entry.dataKey} className="inline-flex items-center gap-1.5">
          <span
            aria-hidden
            className="inline-block h-0.5 w-3 rounded-full"
            style={{ backgroundColor: seriesLineColor(entry.tone, index) }}
          />
          {entry.label}
        </li>
      ))}
    </ul>
  )
}

/**
 * Counters over time for one resolver: internal queries by outcome, and
 * the NXDOMAIN rate on its own chart (a rate and a count never share an
 * axis).
 */
export function DnsTraffic({
  resolvers,
  nodeId,
  range,
  onNodeChange,
  onRangeChange,
}: {
  resolvers: DnsResolverEntry[]
  nodeId: number
  range: DnsTrafficRange
  onNodeChange: (nodeId: number) => void
  onRangeChange: (range: DnsTrafficRange) => void
}) {
  const query = useQuery({
    queryKey: dnsKeys.traffic(nodeId, range),
    queryFn: () => getDnsTraffic(nodeId, range),
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
    meta: { errorTitle: 'Could not load the DNS traffic' },
  })

  const points = query.data ? trafficPoints(query.data.buckets) : []
  const totals = query.data ? trafficTotals(query.data.buckets) : null
  const tick = timeTick(range)

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
        <Select
          value={String(nodeId)}
          onValueChange={(value) => onNodeChange(Number(value))}
        >
          <SelectTrigger className="w-full sm:w-[220px]" aria-label="Node">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {resolvers.map((resolver) => (
              <SelectItem
                key={resolver.node_id}
                value={String(resolver.node_id)}
              >
                {resolver.node_name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          value={range}
          onValueChange={(value) => onRangeChange(value as DnsTrafficRange)}
        >
          <SelectTrigger className="w-full sm:w-[180px]" aria-label="Range">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {DNS_TRAFFIC_RANGES.map((value) => (
              <SelectItem key={value} value={value}>
                {RANGE_LABEL[value]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {query.isError ? (
        <ReadFailure
          resource="DNS traffic"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : (
        <>
          <div className="grid grid-cols-2 gap-2 md:grid-cols-4">
            {totals ? (
              <>
                <Stat
                  label="Internal queries"
                  value={totals.internal.toLocaleString()}
                />
                <Stat
                  label="Failure rate"
                  value={formatPercent(totals.failure_rate)}
                />
                <Stat
                  label="NXDOMAIN rate"
                  value={formatPercent(totals.nxdomain_rate)}
                />
                <Stat
                  label="Forwarded queries"
                  value={totals.external.toLocaleString()}
                />
              </>
            ) : (
              Array.from({ length: 4 }).map((_, index) => (
                <Skeleton key={index} className="h-[66px] w-full" />
              ))
            )}
          </div>

          <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
            <figure className="space-y-2 rounded-md border p-3">
              <figcaption className="flex flex-col gap-1">
                <span className="text-sm font-medium">
                  Internal queries by outcome
                </span>
                <Legend series={QUERY_SERIES} />
              </figcaption>
              {query.isPending ? (
                <Skeleton className="h-[180px] w-full" />
              ) : (
                <TimeChart
                  data={points}
                  xKey="time"
                  height={180}
                  series={QUERY_SERIES}
                  connectNulls={false}
                  xTickFormatter={tick}
                  tooltipValueFormatter={(value) =>
                    Math.round(value).toLocaleString()
                  }
                  emptyMessage="No queries in this range"
                />
              )}
            </figure>
            <figure className="space-y-2 rounded-md border p-3">
              <figcaption className="text-sm font-medium">
                NXDOMAIN rate
              </figcaption>
              {query.isPending ? (
                <Skeleton className="h-[180px] w-full" />
              ) : (
                <TimeChart
                  data={points}
                  xKey="time"
                  height={180}
                  series={{
                    dataKey: 'nxdomain_rate',
                    label: 'NXDOMAIN rate',
                    tone: 'warn',
                  }}
                  connectNulls={false}
                  allowDecimals
                  xTickFormatter={tick}
                  yTickFormatter={(value) => `${value}%`}
                  tooltipValueFormatter={(value) => formatPercent(value)}
                  emptyMessage="No queries in this range"
                />
              )}
            </figure>
          </div>
        </>
      )}
    </div>
  )
}
