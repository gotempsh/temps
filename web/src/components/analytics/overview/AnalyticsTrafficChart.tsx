// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import * as React from 'react'
import { DataSection } from '@/components/data-display/DataSection'
import { ThresholdLineChart } from '@/components/charts/threshold-line-chart'
import { Button } from '@/components/ui/button'
import type { ChartDateRange } from '@/lib/chart-range-selection'
import {
  analyticsTimeline,
  formatAnalyticsTick,
  type AnalyticsTimelineRow,
} from '@/lib/analytics-timeline'
export type AnalyticsMetric = 'events' | 'sessions' | 'visitors'
export function AnalyticsTrafficChart({
  data,
  startDate,
  endDate,
  isLoading,
  error,
  aggregationLevel,
  onAggregationChange,
  onZoom,
  selectedRange,
}: {
  data?: AnalyticsTimelineRow[]
  startDate?: Date
  endDate?: Date
  isLoading: boolean
  error?: unknown
  aggregationLevel: AnalyticsMetric
  onAggregationChange: (value: AnalyticsMetric) => void
  onZoom?: (from: Date, to: Date) => void
  selectedRange?: ChartDateRange | null
}) {
  const chartData = React.useMemo(
    () =>
      startDate && endDate
        ? analyticsTimeline(data ?? [], startDate, endDate)
        : [],
    [data, startDate, endDate]
  )

  const getAggregationLabel = () => {
    switch (aggregationLevel) {
      case 'events':
        return 'Page Views'
      case 'sessions':
        return 'Sessions'
      case 'visitors':
        return 'Visitors'
    }
  }

  const getChartTitle = () => {
    if (!startDate || !endDate) return getAggregationLabel()

    const rangeInDays = Math.ceil(
      (endDate.getTime() - startDate.getTime()) / (1000 * 60 * 60 * 24)
    )
    const sameDay = startDate.toDateString() === endDate.toDateString()

    if (sameDay || rangeInDays <= 1) {
      return `Hourly ${getAggregationLabel()}`
    } else {
      return getAggregationLabel()
    }
  }

  return (
    <DataSection
      title={getChartTitle()}
      description={
        onZoom ? 'Drag across the chart to inspect a time window.' : undefined
      }
      actions={
        <div
          role="group"
          aria-label="Traffic metric"
          className="flex items-center gap-1 rounded-md bg-muted p-1"
        >
          {(['events', 'sessions', 'visitors'] as const).map((metric) => (
            <Button
              key={metric}
              type="button"
              variant={aggregationLevel === metric ? 'secondary' : 'ghost'}
              size="sm"
              aria-pressed={aggregationLevel === metric}
              onClick={() => onAggregationChange(metric)}
            >
              {metric === 'events'
                ? 'Page views'
                : metric === 'sessions'
                  ? 'Sessions'
                  : 'Visitors'}
            </Button>
          ))}
        </div>
      }
    >
      {isLoading ? (
        <div className="h-[250px] w-full flex items-center justify-center">
          <div className="text-sm text-muted-foreground">
            Loading chart data...
          </div>
        </div>
      ) : error ? (
        <div className="h-[250px] w-full flex items-center justify-center">
          <div className="text-sm text-red-500">Failed to load chart data</div>
        </div>
      ) : !chartData.length ? (
        <div className="h-[250px] w-full flex items-center justify-center">
          <div className="text-sm text-muted-foreground">
            No data available for the selected period
          </div>
        </div>
      ) : (
        <ThresholdLineChart
          data={chartData}
          xKey="timestamp"
          series={{
            dataKey: 'count',
            label: getAggregationLabel(),
            tone: 'neutral',
          }}
          height={250}
          allowDecimals={false}
          xTickFormatter={(value) =>
            formatAnalyticsTick(Number(value), startDate, endDate)
          }
          yTickFormatter={(value) => value.toLocaleString()}
          tooltipValueFormatter={(value) => value.toLocaleString()}
          selectionKey="timestamp"
          selectedRange={selectedRange}
          onRangeSelect={onZoom}
        />
      )}
    </DataSection>
  )
}
