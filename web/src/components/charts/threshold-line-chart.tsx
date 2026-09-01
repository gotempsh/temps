// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ReactNode, useCallback, useMemo, useRef, useState } from 'react'
import {
  Area,
  CartesianGrid,
  ComposedChart,
  Line,
  ReferenceArea,
  ReferenceLine,
  XAxis,
  YAxis,
} from 'recharts'
import {
  ChartConfig,
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
} from '@/components/ui/chart'
import { cn } from '@/lib/utils'
import {
  formatChartDateRange,
  orderedChartDateRange,
} from '@/lib/chart-range-selection'
import type { ChartDateRange } from '@/lib/chart-range-selection'
import type { MetricTone } from './metric-sparkline'
import {
  SERIES_STROKE,
  THRESHOLD_STROKE,
  seriesLineColor,
} from './chart-colors'

export type ThresholdLineSeries = {
  /** Data key on each point — what to plot. */
  dataKey: string
  /** Stroke tone for the line. Defaults to `neutral` (primary). */
  tone?: MetricTone | 'primary'
  /** Human-readable series label for the tooltip. */
  label: string
}

export type ThresholdBand = {
  /** Y value to draw the reference line at. */
  value: number
  /** Tone picks the color; "good" is emerald, "poor" is red, "warn" is amber. */
  tone: MetricTone
  /** Optional label rendered at the end of the reference line. */
  label?: string
}

/** A shaded horizontal band (e.g. an anomaly rule's expected `[lower, upper]`). */
export type ThresholdBandArea = {
  lower: number
  upper: number
  tone: MetricTone
  label?: string
}

/** A vertical event marker (e.g. a deployment) at a categorical x value. */
export type ThresholdMarker = {
  /** Must match a data point's `xKey` value (the categorical x axis). */
  x: string
  /** Short label drawn at the top of the line (e.g. a commit hash). */
  label?: string
  /** Tooltip text (e.g. the commit message + time). */
  title?: string
}

/**
 * A time-varying "expected range" band (e.g. an anomaly rule's seasonal band),
 * read from per-point data keys. `lowerKey` is the band floor and `spanKey` the
 * band height (upper − lower), so the chart shades [lower, lower + span]
 * (recharts draws a band as a transparent base area + a filled span on top).
 * `breachKey`, when set, marks the points that left the band — the anomaly.
 */
export type ThresholdBandSeries = {
  lowerKey: string
  spanKey: string
  breachKey?: string
  tone: MetricTone
}

interface ThresholdLineChartProps {
  data: any[]
  xKey: string
  /**
   * A single line (unchanged single-series behavior), or an array to render a
   * breakdown — one `<Line>` per entry, each reading its own `dataKey` off the
   * same wide-format `data` rows.
   */
  series: ThresholdLineSeries | ThresholdLineSeries[]
  /** Horizontal reference lines drawn across the chart for pass/fail bands. */
  thresholds?: ThresholdBand[]
  /** Shaded horizontal bands (e.g. an anomaly rule's expected range). */
  bands?: ThresholdBandArea[]
  /** Vertical event markers (e.g. deployments) at categorical x values. */
  markers?: ThresholdMarker[]
  /** Time-varying expected-range band drawn behind the line (anomaly band). */
  bandSeries?: ThresholdBandSeries
  /** Height of the chart in px. Defaults to 300. */
  height?: number
  /** Format the Y-axis ticks (e.g. "2.5s"). */
  yTickFormatter?: (value: number) => string
  /** Format categorical X-axis ticks without changing their unique values. */
  xTickFormatter?: (value: string | number) => string
  /** Format the tooltip value. */
  tooltipValueFormatter?: (value: number) => string
  /** Extra content rendered inside the tooltip under the value. */
  tooltipFooter?: (value: number) => ReactNode
  /**
   * Message rendered instead of the chart when there aren't enough points
   * to draw a line. Defaults to a generic fallback.
   */
  emptyMessage?: ReactNode
  /** Raw timestamp key used when the user drags across the chart. */
  selectionKey?: string
  /** Called with the ordered timestamp range after a multi-point drag. */
  onRangeSelect?: (from: Date, to: Date) => void
  /** Confirmed-but-not-yet-applied range to keep highlighted on the chart. */
  selectedRange?: ChartDateRange | null
  className?: string
}

function SelectedRangeLabel({
  viewBox,
  range,
}: {
  viewBox?: { x?: number; y?: number; width?: number }
  range: ChartDateRange
}) {
  const x = viewBox?.x ?? 0
  const y = viewBox?.y ?? 0
  const width = viewBox?.width ?? 0
  const text = formatChartDateRange(range)
  const labelWidth = Math.min(Math.max(text.length * 6.4 + 20, 190), 330)
  const center = x + width / 2

  return (
    <g className="pointer-events-none">
      <rect
        x={center - labelWidth / 2}
        y={y + 8}
        width={labelWidth}
        height={24}
        rx={6}
        fill="var(--popover)"
        stroke="var(--primary)"
        strokeOpacity={0.45}
      />
      <text
        x={center}
        y={y + 24}
        textAnchor="middle"
        fill="var(--popover-foreground)"
        fontFamily="var(--font-mono)"
        fontSize={10}
        fontWeight={500}
      >
        {text}
      </text>
    </g>
  )
}

function MarkerLabel({
  viewBox,
  text,
  title,
}: {
  viewBox?: { x?: number; y?: number }
  text: string
  title?: string
}) {
  const x = viewBox?.x ?? 0
  const y = viewBox?.y ?? 0

  return (
    <text x={x + 4} y={y + 10} fill="var(--chart-5)" fontSize={9}>
      {title && <title>{title}</title>}
      {text}
    </text>
  )
}

/**
 * Themed line chart with optional horizontal threshold reference lines (e.g.
 * Core Web Vitals "Good" / "Poor" bands). `series` is either a single line
 * (today's shape) or an array to render a label breakdown as one line per
 * entry, all reading their own `dataKey` off the same wide-format `data` rows.
 *
 * Built on `ChartContainer` so grid, axis, and tooltip automatically follow
 * the app theme in both light and dark mode.
 */
export function ThresholdLineChart({
  data,
  xKey,
  series,
  thresholds = [],
  bands = [],
  markers = [],
  bandSeries,
  height = 300,
  yTickFormatter,
  xTickFormatter,
  tooltipValueFormatter,
  tooltipFooter,
  emptyMessage,
  selectionKey,
  onRangeSelect,
  selectedRange,
  className,
}: ThresholdLineChartProps) {
  const isMulti = Array.isArray(series)
  const seriesList = isMulti ? series : [series]
  const [selectionStartX, setSelectionStartX] = useState<
    string | number | null
  >(null)
  const [selectionEndX, setSelectionEndX] = useState<string | number | null>(
    null
  )
  const selectionStartValue = useRef<unknown>(null)
  const selectionEndValue = useRef<unknown>(null)
  const isSelecting = useRef(false)

  const clearSelection = useCallback(() => {
    isSelecting.current = false
    selectionStartValue.current = null
    selectionEndValue.current = null
    setSelectionStartX(null)
    setSelectionEndX(null)
  }, [])

  const selectionPointFromEvent = useCallback(
    (event: any) => {
      const payload = event?.activePayload?.[0]?.payload
      if (payload) return payload

      const activeIndex = Number(event?.activeIndex)
      if (
        Number.isInteger(activeIndex) &&
        activeIndex >= 0 &&
        activeIndex < data.length
      ) {
        return data[activeIndex]
      }

      if (event?.activeLabel != null) {
        return data.find((point) => point?.[xKey] === event.activeLabel)
      }

      return undefined
    },
    [data, xKey]
  )

  const handleMouseDown = useCallback(
    (event: any) => {
      if (!selectionKey || !onRangeSelect) return
      const payload = selectionPointFromEvent(event)
      const xValue = payload?.[xKey]
      const selectionValue = payload?.[selectionKey]
      if (
        (typeof xValue !== 'string' && typeof xValue !== 'number') ||
        selectionValue == null
      ) {
        return
      }

      isSelecting.current = true
      selectionStartValue.current = selectionValue
      selectionEndValue.current = null
      setSelectionStartX(xValue)
      setSelectionEndX(null)
    },
    [onRangeSelect, selectionKey, selectionPointFromEvent, xKey]
  )

  const handleMouseMove = useCallback(
    (event: any) => {
      if (!isSelecting.current || !selectionKey) return
      const payload = selectionPointFromEvent(event)
      const xValue = payload?.[xKey]
      const selectionValue = payload?.[selectionKey]
      if (
        (typeof xValue !== 'string' && typeof xValue !== 'number') ||
        selectionValue == null
      ) {
        return
      }

      selectionEndValue.current = selectionValue
      setSelectionEndX(xValue)
    },
    [selectionKey, selectionPointFromEvent, xKey]
  )

  const handleMouseUp = useCallback(() => {
    if (!isSelecting.current || !onRangeSelect) {
      clearSelection()
      return
    }

    const range = orderedChartDateRange(
      selectionStartValue.current,
      selectionEndValue.current
    )
    clearSelection()
    if (range) onRangeSelect(range.from, range.to)
  }, [clearSelection, onRangeSelect])

  const selectedRangeX = useMemo(() => {
    if (!selectedRange || !selectionKey) return null

    const points = data.flatMap((point) => {
      const rawTimestamp = point?.[selectionKey]
      const timestamp = new Date(rawTimestamp).getTime()
      const x = point?.[xKey]
      return Number.isNaN(timestamp) ||
        (typeof x !== 'string' && typeof x !== 'number')
        ? []
        : [{ timestamp, x }]
    })
    if (points.length === 0) return null

    const closestX = (target: number) =>
      points.reduce((closest, point) =>
        Math.abs(point.timestamp - target) <
        Math.abs(closest.timestamp - target)
          ? point
          : closest
      ).x

    return {
      from: closestX(selectedRange.from.getTime()),
      to: closestX(selectedRange.to.getTime()),
    }
  }, [data, selectedRange, selectionKey, xKey])

  const config: ChartConfig = {}
  seriesList.forEach((s, i) => {
    config[s.dataKey] = {
      label: s.label,
      color: isMulti
        ? seriesLineColor(s.tone, i)
        : s.tone
          ? SERIES_STROKE[s.tone]
          : SERIES_STROKE.primary,
    }
  })

  const validCounts = seriesList.map((s) =>
    data.reduce((n, p) => {
      const v = p?.[s.dataKey]
      return v === null || v === undefined ? n : n + 1
    }, 0)
  )
  const maxValidCount = Math.max(0, ...validCounts)

  if (maxValidCount < 2) {
    return (
      <div
        className={cn(
          'flex w-full items-center justify-center rounded-md border border-dashed text-sm text-muted-foreground',
          className
        )}
        style={{ height }}
      >
        {emptyMessage ?? (
          <div className="flex flex-col items-center gap-1 px-4 text-center">
            <span className="font-medium text-foreground">
              Not enough data to chart
            </span>
            <span className="text-xs">
              {maxValidCount === 0
                ? 'No samples in this range.'
                : 'Only one sample recorded — a trend needs at least two.'}
            </span>
          </div>
        )}
      </div>
    )
  }

  // Include threshold lines in the Y-axis domain so they're always visible.
  // Recharts auto-fits to data, which pushes out-of-range threshold lines
  // off the chart — e.g. a 488ms LCP never reveals the 2500ms/4000ms bands.
  const numericValues = data.flatMap((p) =>
    seriesList
      .map((s) => p?.[s.dataKey])
      .filter((v): v is number => typeof v === 'number')
  )
  const dataMax = numericValues.length ? Math.max(...numericValues) : 0
  const dataMin = numericValues.length ? Math.min(...numericValues) : 0
  const thresholdMax = thresholds.reduce(
    (m, t) => Math.max(m, t.value),
    dataMax
  )
  // Keep shaded band edges inside the visible Y range too.
  const bandMax = bands.reduce((m, b) => Math.max(m, b.upper), thresholdMax)
  const bandMin = bands.reduce((m, b) => Math.min(m, b.lower), dataMin)
  // A time-varying band (anomaly) can dip below / rise above the line — widen
  // the domain to its envelope so the whole band stays on-chart.
  let envMax = bandMax
  let envMin = bandMin
  if (bandSeries) {
    for (const p of data) {
      const lo = p?.[bandSeries.lowerKey]
      const sp = p?.[bandSeries.spanKey]
      if (typeof lo === 'number' && typeof sp === 'number') {
        envMin = Math.min(envMin, lo)
        envMax = Math.max(envMax, lo + sp)
      }
    }
  }
  const yMin = Math.min(0, envMin)
  const yMax = envMax === yMin ? yMin + 1 : envMax * 1.1

  return (
    <ChartContainer
      config={config}
      className={cn(
        'aspect-auto w-full',
        selectionKey && onRangeSelect && 'cursor-crosshair select-none',
        className
      )}
      style={{ height }}
    >
      <ComposedChart
        data={data}
        margin={{ top: 12, right: 24, left: 8, bottom: 0 }}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={clearSelection}
      >
        <CartesianGrid
          strokeDasharray="3 3"
          vertical={false}
          className="stroke-border"
          strokeOpacity={0.6}
        />
        {/* Anomaly "expected range" band: a transparent base at `lower`, with the
            filled span (upper − lower) stacked on top — drawn first so it sits
            behind the line. Kept as two sibling <Area>s (NOT wrapped in a
            fragment — recharts only detects cartesian children at the top level).
            Tooltip excludes both via tooltipType="none". */}
        {bandSeries && (
          <Area
            key="anomaly-band-base"
            type="monotone"
            dataKey={bandSeries.lowerKey}
            stackId="anomaly-band"
            stroke="none"
            fill="none"
            connectNulls
            isAnimationActive={false}
            activeDot={false}
            tooltipType="none"
            legendType="none"
          />
        )}
        {bandSeries && (
          <Area
            key="anomaly-band-span"
            type="monotone"
            dataKey={bandSeries.spanKey}
            stackId="anomaly-band"
            stroke="none"
            fill={THRESHOLD_STROKE[bandSeries.tone]}
            fillOpacity={0.12}
            connectNulls
            isAnimationActive={false}
            activeDot={false}
            tooltipType="none"
            legendType="none"
          />
        )}
        <XAxis
          dataKey={xKey}
          tickLine={false}
          axisLine={false}
          tickMargin={8}
          minTickGap={32}
          tickFormatter={xTickFormatter}
          className="text-xs"
        />
        <YAxis
          tickLine={false}
          axisLine={false}
          tickMargin={8}
          width={52}
          domain={[yMin, yMax]}
          tickFormatter={yTickFormatter}
          className="text-xs"
        />
        <ChartTooltip
          cursor={{ strokeDasharray: '3 3' }}
          content={
            <ChartTooltipContent
              indicator="line"
              formatter={(value, _name, item) => {
                const num = value as number
                // Only the breakdown case needs a per-line label — a single
                // series already conveys "what" via the tile title.
                const seriesLabel = isMulti
                  ? (config[String(item?.dataKey)]?.label as string | undefined)
                  : undefined
                return (
                  <div className="flex flex-col gap-0.5">
                    {seriesLabel && (
                      <span className="font-mono text-[10px] text-muted-foreground">
                        {seriesLabel}
                      </span>
                    )}
                    <span className="font-mono font-medium text-foreground">
                      {tooltipValueFormatter
                        ? tooltipValueFormatter(num)
                        : num.toLocaleString()}
                    </span>
                    {tooltipFooter ? (
                      <span className="text-[10px] text-muted-foreground">
                        {tooltipFooter(num)}
                      </span>
                    ) : null}
                  </div>
                )
              }}
            />
          }
        />
        {bands.map((b, idx) => (
          <ReferenceArea
            key={`band-${idx}`}
            y1={b.lower}
            y2={b.upper}
            fill={THRESHOLD_STROKE[b.tone]}
            fillOpacity={0.1}
            stroke="none"
            label={
              b.label
                ? {
                    value: b.label,
                    position: 'insideTopRight',
                    fill: THRESHOLD_STROKE[b.tone],
                    fontSize: 10,
                  }
                : undefined
            }
          />
        ))}
        {selectionStartX != null && selectionEndX != null && (
          <ReferenceArea
            x1={selectionStartX}
            x2={selectionEndX}
            fill="var(--primary)"
            fillOpacity={0.1}
            stroke="var(--primary)"
            strokeOpacity={0.35}
          />
        )}
        {selectionStartX == null && selectedRangeX && selectedRange && (
          <ReferenceArea
            x1={selectedRangeX.from}
            x2={selectedRangeX.to}
            fill="var(--primary)"
            fillOpacity={0.14}
            stroke="var(--primary)"
            strokeOpacity={0.55}
            strokeWidth={1}
            label={<SelectedRangeLabel range={selectedRange} />}
          />
        )}
        {thresholds.map((t, idx) => (
          <ReferenceLine
            key={`${t.tone}-${idx}`}
            y={t.value}
            stroke={THRESHOLD_STROKE[t.tone]}
            strokeDasharray="4 4"
            strokeOpacity={0.7}
            label={
              t.label
                ? {
                    value: t.label,
                    position: 'right',
                    fill: THRESHOLD_STROKE[t.tone],
                    fontSize: 10,
                  }
                : undefined
            }
          />
        ))}
        {markers.map((m, idx) => (
          <ReferenceLine
            key={`marker-${idx}`}
            x={m.x}
            stroke="var(--chart-5)"
            strokeDasharray="3 3"
            strokeOpacity={0.85}
            label={
              m.label ? (
                <MarkerLabel text={m.label} title={m.title} />
              ) : undefined
            }
          />
        ))}
        {seriesList.map((s, i) => (
          <Line
            key={s.dataKey}
            type="monotone"
            dataKey={s.dataKey}
            stroke={`var(--color-${s.dataKey})`}
            strokeWidth={2}
            // A breakdown can have many overlapping lines — per-point dots
            // just add clutter there; the single-series dot-when-sparse
            // behavior is unchanged.
            dot={
              !isMulti && validCounts[i] <= 8 ? { r: 3, strokeWidth: 0 } : false
            }
            activeDot={{ r: 4, strokeWidth: 0 }}
            connectNulls
            isAnimationActive={false}
          />
        ))}
        {/* Breach markers: dots at the points that left the band — the anomaly
            itself. A stroke-less Line over the VALUE series with a custom dot
            that renders only where `breachKey` is set (recharts' Scatter plots
            null points at the top, so it can't be used to mark a sparse subset).
            Bands are only ever paired with the single-series path, so the
            first (only) series is the right one to key the overlay off. */}
        {bandSeries?.breachKey && (
          <Line
            dataKey={seriesList[0].dataKey}
            stroke="none"
            legendType="none"
            tooltipType="none"
            isAnimationActive={false}
            activeDot={false}
            dot={(props: any) => {
              const breaching =
                props?.payload?.[bandSeries.breachKey as string] != null
              return breaching ? (
                <circle
                  key={`breach-${props.index}`}
                  cx={props.cx}
                  cy={props.cy}
                  r={3.5}
                  fill="var(--destructive)"
                  stroke="var(--background)"
                  strokeWidth={1}
                />
              ) : (
                <g key={`breach-empty-${props.index}`} />
              )
            }}
          />
        )}
      </ComposedChart>
    </ChartContainer>
  )
}
