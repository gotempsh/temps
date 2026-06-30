import { ReactNode } from 'react'
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
import type { MetricTone } from './metric-sparkline'

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
  series: ThresholdLineSeries
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
  /** Format the tooltip value. */
  tooltipValueFormatter?: (value: number) => string
  /** Extra content rendered inside the tooltip under the value. */
  tooltipFooter?: (value: number) => ReactNode
  /**
   * Message rendered instead of the chart when there aren't enough points
   * to draw a line. Defaults to a generic fallback.
   */
  emptyMessage?: ReactNode
  className?: string
}

const SERIES_STROKE: Record<NonNullable<ThresholdLineSeries['tone']>, string> = {
  good: 'var(--chart-2)',
  warn: 'var(--chart-3)',
  poor: 'var(--chart-4)',
  neutral: 'var(--chart-1)',
  primary: 'var(--chart-1)',
}

const THRESHOLD_STROKE: Record<MetricTone, string> = {
  good: 'var(--chart-2)',
  warn: 'var(--chart-3)',
  poor: 'var(--chart-4)',
  neutral: 'var(--muted-foreground)',
}

/**
 * Themed single-series line chart with optional horizontal threshold
 * reference lines (e.g. Core Web Vitals "Good" / "Poor" bands).
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
  tooltipValueFormatter,
  tooltipFooter,
  emptyMessage,
  className,
}: ThresholdLineChartProps) {
  const tone = series.tone ?? 'primary'
  const stroke = SERIES_STROKE[tone]

  const config: ChartConfig = {
    [series.dataKey]: {
      label: series.label,
      color: stroke,
    },
  }

  const validCount = data.reduce((n, p) => {
    const v = p?.[series.dataKey]
    return v === null || v === undefined ? n : n + 1
  }, 0)

  if (validCount < 2) {
    return (
      <div
        className={cn(
          'flex w-full items-center justify-center rounded-md border border-dashed text-sm text-muted-foreground',
          className,
        )}
        style={{ height }}
      >
        {emptyMessage ?? (
          <div className="flex flex-col items-center gap-1 px-4 text-center">
            <span className="font-medium text-foreground">
              Not enough data to chart
            </span>
            <span className="text-xs">
              {validCount === 0
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
  const numericValues = data
    .map((p) => p?.[series.dataKey])
    .filter((v): v is number => typeof v === 'number')
  const dataMax = numericValues.length ? Math.max(...numericValues) : 0
  const dataMin = numericValues.length ? Math.min(...numericValues) : 0
  const thresholdMax = thresholds.reduce(
    (m, t) => Math.max(m, t.value),
    dataMax,
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
  const yMax = envMax * 1.1
  const yMin = Math.min(0, envMin)

  return (
    <ChartContainer
      config={config}
      className={cn('aspect-auto w-full', className)}
      style={{ height }}
    >
      <ComposedChart
        data={data}
        margin={{ top: 12, right: 24, left: 8, bottom: 0 }}
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
              formatter={(value) => {
                const num = value as number
                return (
                  <div className="flex flex-col gap-0.5">
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
              m.label
                ? {
                    value: m.label,
                    position: 'insideTopRight',
                    fill: 'var(--chart-5)',
                    fontSize: 9,
                  }
                : undefined
            }
          />
        ))}
        <Line
          type="monotone"
          dataKey={series.dataKey}
          stroke={`var(--color-${series.dataKey})`}
          strokeWidth={2}
          dot={validCount <= 8 ? { r: 3, strokeWidth: 0 } : false}
          activeDot={{ r: 4, strokeWidth: 0 }}
          connectNulls
          isAnimationActive={false}
        />
        {/* Breach markers: dots at the points that left the band — the anomaly
            itself. A stroke-less Line over the VALUE series with a custom dot
            that renders only where `breachKey` is set (recharts' Scatter plots
            null points at the top, so it can't be used to mark a sparse subset). */}
        {bandSeries?.breachKey && (
          <Line
            dataKey={series.dataKey}
            stroke="none"
            legendType="none"
            tooltipType="none"
            isAnimationActive={false}
            activeDot={false}
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
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
