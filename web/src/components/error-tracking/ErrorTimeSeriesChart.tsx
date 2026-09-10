// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getErrorTimeSeriesOptions,
  // Deploy events overlaid as vertical markers — "did a deploy cause this?"
  // Same endpoint + chart component MetricsExplorer uses for its OTel charts.
  getProjectDeploymentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import {
  ThresholdLineChart,
  type ThresholdMarker,
} from '@/components/charts/threshold-line-chart'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import React, { useMemo, useState } from 'react'

interface ErrorTimeSeriesChartProps {
  project: ProjectResponse
  startDate: Date
  endDate: Date
  environmentId?: number
}

export function ErrorTimeSeriesChart({
  project,
  startDate,
  endDate,
  environmentId,
}: ErrorTimeSeriesChartProps) {
  // Calculate time range in hours to determine appropriate bucket
  const timeRangeHours = useMemo(() => {
    return (endDate.getTime() - startDate.getTime()) / (1000 * 60 * 60)
  }, [startDate, endDate])

  // Auto-determine bucket based on time range, but allow manual override
  const defaultBucket = useMemo(() => {
    if (timeRangeHours <= 1) return '5m' // Last hour: 5 minutes
    if (timeRangeHours <= 24) return '1h' // Last 24 hours: 1 hour
    return '1d' // 7+ days: 1 day
  }, [timeRangeHours])

  const [selectedBucket, setSelectedBucket] = useState<'5m' | '1h' | '1d'>(
    defaultBucket
  )

  // Update selected bucket when time range changes
  React.useEffect(() => {
    setSelectedBucket(defaultBucket)
  }, [defaultBucket])

  const { data, isLoading, error } = useQuery({
    ...getErrorTimeSeriesOptions({
      path: {
        project_id: project.id,
      },
      query: {
        start_time: startDate.toISOString(),
        end_time: endDate.toISOString(),
        bucket: selectedBucket,
        environment_id: environmentId,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const chartData = useMemo(() => {
    if (!data || data.length === 0) return []

    // Format date based on bucket size
    const dateFormat =
      selectedBucket === '5m'
        ? 'HH:mm' // 5 minutes: show time only
        : selectedBucket === '1h'
          ? 'MMM dd HH:mm' // 1 hour: show date and time
          : 'MMM dd' // 1 day: show date only

    return data.map((point) => ({
      timestamp: point.timestamp,
      date: format(new Date(point.timestamp), dateFormat),
      count: point.count,
    }))
  }, [data, selectedBucket])

  const totalErrors = useMemo(() => {
    if (!data) return 0
    return data.reduce((sum, point) => sum + point.count, 0)
  }, [data])

  // ── Deploy markers ──
  // Overlay deploy events that fall inside the visible window as vertical
  // lines, snapped to the nearest chart bucket — mirrors MetricsExplorer's
  // deploy-marker overlay so "did a deploy cause this?" works the same way
  // across every chart in the app.
  const deploysQuery = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: {
        per_page: 50,
        ...(environmentId != null ? { environment_id: environmentId } : {}),
      },
    }),
    enabled: !!project.id,
  })
  const deployMarkers = useMemo<ThresholdMarker[]>(() => {
    const deploys = deploysQuery.data?.deployments ?? []
    if (chartData.length === 0 || deploys.length === 0) return []
    const fromMs = startDate.getTime()
    const toMs = endDate.getTime()
    // Timestamps come back in seconds; normalise to ms.
    const toMs10 = (n: number) => (n < 1e12 ? n * 1000 : n)
    const bucketMs = chartData.map((p) => new Date(p.timestamp).getTime())
    // Multiple deploys can snap to the same bucket — recharts renders one
    // <ReferenceLine> per marker, and two labels at an identical x stack
    // directly on top of each other and render as garbled overlapping text.
    // Group by bucket and collapse each group into a single marker.
    const byBucket = new Map<
      number,
      { label: string; title?: string; count: number }
    >()
    for (const d of deploys) {
      const at = d.finished_at ?? d.started_at ?? d.created_at
      const ts = toMs10(at)
      if (ts < fromMs || ts > toMs) continue
      // Snap to the nearest bucket (the categorical x axis can't take a raw ts).
      let best = 0
      let bestDiff = Infinity
      for (let i = 0; i < bucketMs.length; i++) {
        const diff = Math.abs(bucketMs[i] - ts)
        if (diff < bestDiff) {
          bestDiff = diff
          best = i
        }
      }
      const existing = byBucket.get(best)
      if (existing) {
        existing.count += 1
      } else {
        byBucket.set(best, {
          label: d.commit_hash ? d.commit_hash.slice(0, 7) : 'deploy',
          title: d.commit_message ?? undefined,
          count: 1,
        })
      }
    }
    const markers: ThresholdMarker[] = []
    for (const [best, m] of byBucket) {
      markers.push({
        x: chartData[best].date,
        label: m.count > 1 ? `${m.label} +${m.count - 1}` : m.label,
        title: m.title,
      })
    }
    return markers
  }, [deploysQuery.data, chartData, startDate, endDate])

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between">
          <div>
            <CardTitle>Error Time Series</CardTitle>
            <CardDescription>
              {format(startDate, 'LLL dd, y')} - {format(endDate, 'LLL dd, y')}
            </CardDescription>
          </div>
          <div className="flex gap-2">
            {timeRangeHours <= 1 && (
              <Button
                variant={selectedBucket === '5m' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setSelectedBucket('5m')}
              >
                5 Min
              </Button>
            )}
            {timeRangeHours <= 24 && (
              <Button
                variant={selectedBucket === '1h' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setSelectedBucket('1h')}
              >
                Hourly
              </Button>
            )}
            <Button
              variant={selectedBucket === '1d' ? 'default' : 'outline'}
              size="sm"
              onClick={() => setSelectedBucket('1d')}
            >
              Daily
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="space-y-4 py-4">
            <Skeleton className="h-[400px] w-full" />
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground mb-2">
              Failed to load error time series
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => window.location.reload()}
            >
              Try again
            </Button>
          </div>
        ) : !chartData.length || !data || data.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <p className="text-sm text-muted-foreground">
              No error data available for the selected period
            </p>
          </div>
        ) : (
          <div>
            <div className="mb-4">
              <div className="text-2xl font-bold">
                {totalErrors.toLocaleString()}
              </div>
              <p className="text-sm text-muted-foreground">
                Total errors in period
              </p>
            </div>
            <ThresholdLineChart
              data={chartData}
              xKey="date"
              series={{ dataKey: 'count', label: 'Errors', tone: 'neutral' }}
              markers={deployMarkers}
              height={400}
              tooltipValueFormatter={(v) => v.toLocaleString()}
              yTickFormatter={(v) => v.toLocaleString()}
            />
          </div>
        )}
      </CardContent>
    </Card>
  )
}
