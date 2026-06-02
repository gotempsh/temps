import { getAiAgentTimelineOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ChartConfig, ChartContainer } from '@/components/ui/chart'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import * as React from 'react'
import { Bar, BarChart, CartesianGrid, Tooltip, XAxis, YAxis } from 'recharts'

interface AiAgentsTimelineChartProps {
  project: ProjectResponse
  startDate: Date | undefined
  endDate: Date | undefined
  environment: number | undefined
}

/**
 * Stable colour palette for the stacked series. The first entries are
 * brand-leaning for the providers people recognise; the rest cycle through the
 * shared chart vars so any long-tail provider/agent still gets a distinct hue.
 */
const SERIES_COLORS = [
  '#10a37f', // OpenAI green
  '#d97757', // Anthropic clay
  '#20808d', // Perplexity teal
  '#4285f4', // Google blue
  '#0078d4', // Microsoft/Bing blue
  '#ff9900', // Amazon orange
  '#1877f2', // Meta blue
  '#000000', // xAI / generic black
  'var(--chart-5)',
  'var(--chart-2)',
  'var(--chart-3)',
  'var(--chart-4)',
  'var(--chart-1)',
  '#e5484d',
  '#8a63d2',
  '#f5a623',
]

/** Round-bucket label that adapts to the bucket width. */
function formatBucketTick(iso: string, bucket: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  // Sub-day buckets show time; day+ buckets show the date.
  if (bucket.includes('minute') || bucket.includes('hour')) {
    return format(d, 'MMM d, HH:mm')
  }
  return format(d, 'MMM d')
}

type PivotRow = { bucket: string } & Record<string, number | string>

/**
 * Provider/agent names can contain spaces and dots ("Common Crawl", "You.com")
 * which are invalid in CSS custom-property names (`--color-<key>`). Map each raw
 * key to a safe slug used for the dataKey + colour var; keep the raw name for
 * the legend/tooltip label.
 */
function slugifyKey(key: string): string {
  return key.replace(/[^a-zA-Z0-9_-]/g, '_')
}

interface TooltipEntry {
  dataKey?: string | number
  value?: number
  color?: string
  payload?: Record<string, unknown>
}

/**
 * Per-bucket tooltip: lists every series that has traffic in the hovered bucket
 * (descending by count, zeros hidden) with a colour swatch and number, plus a
 * total row. Answers "exactly how many requests did each agent/provider make in
 * this window?" without the noise of the ~16-series default tooltip.
 */
function AiTimelineTooltip({
  active,
  payload,
  bucket,
  chartConfig,
  colorBySlug,
}: {
  active?: boolean
  payload?: TooltipEntry[]
  bucket: string
  chartConfig: ChartConfig
  colorBySlug: Record<string, string>
}) {
  if (!active || !payload?.length) return null

  const rows = payload
    .map((p) => ({
      key: String(p.dataKey ?? ''),
      value: typeof p.value === 'number' ? p.value : 0,
      color: colorBySlug[String(p.dataKey ?? '')] ?? p.color,
    }))
    .filter((r) => r.value > 0)
    .sort((a, b) => b.value - a.value)

  if (rows.length === 0) return null

  const total = rows.reduce((s, r) => s + r.value, 0)
  const bucketIso = payload[0]?.payload?.bucket
  const label = bucketIso ? formatBucketTick(String(bucketIso), bucket) : ''

  return (
    <div className="min-w-[11rem] rounded-lg border bg-background px-3 py-2 text-xs shadow-md">
      <div className="mb-1.5 font-medium text-foreground">{label}</div>
      <div className="space-y-1">
        {rows.map((r) => (
          <div key={r.key} className="flex items-center justify-between gap-3">
            <span className="flex items-center gap-1.5 text-muted-foreground">
              <span
                className="size-2 shrink-0 rounded-[2px]"
                style={{ backgroundColor: r.color }}
              />
              {chartConfig[r.key]?.label ?? r.key}
            </span>
            <span className="font-medium tabular-nums text-foreground">
              {r.value.toLocaleString()}
            </span>
          </div>
        ))}
      </div>
      <div className="mt-1.5 flex items-center justify-between gap-3 border-t pt-1.5">
        <span className="text-muted-foreground">Total</span>
        <span className="font-semibold tabular-nums text-foreground">
          {total.toLocaleString()}
        </span>
      </div>
    </div>
  )
}

/**
 * "AI agents over time" — a stacked bar chart of crawler request volume per
 * time bucket, split by provider or agent. Reads the proxy-log AI timeline
 * endpoint (same source as the AI agent tables, just bucketed) and pivots the
 * `(bucket, key, count)` rows into one stacked series per `key`.
 */
export function AiAgentsTimelineChart({
  project,
  startDate,
  endDate,
  environment,
}: AiAgentsTimelineChartProps) {
  const [groupBy, setGroupBy] = React.useState<'provider' | 'agent'>('provider')

  const { data, isLoading, error } = useQuery({
    ...getAiAgentTimelineOptions({
      query: {
        project_id: project.id,
        environment_id: environment,
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
        group_by: groupBy,
      },
    }),
    enabled: !!startDate && !!endDate,
  })

  const bucket = data?.bucket ?? '1 hour'

  // Pivot (bucket, key, count) rows into one object per bucket with a numeric
  // field per series key, ranking keys by total volume so the densest series
  // gets the first (most distinct) colour and a stable stack order.
  const { chartData, seriesKeys, chartConfig, colorBySlug } = React.useMemo(() => {
    const items = data?.items ?? []
    if (items.length === 0) {
      return {
        chartData: [] as PivotRow[],
        seriesKeys: [] as string[],
        chartConfig: {} as ChartConfig,
        colorBySlug: {} as Record<string, string>,
      }
    }

    // Aggregate by the SLUG so the dataKey, CSS colour var, and config key all
    // agree; remember the raw display name per slug for the legend/tooltip.
    // The server returns the full bucket spine (gap-filled), one row per
    // (bucket, key) for buckets with data plus an empty-key marker row for each
    // empty bucket so the x-axis stays continuous. Key buckets by epoch ms.
    const totals = new Map<string, number>()
    const labelBySlug = new Map<string, string>()
    const byBucket = new Map<number, PivotRow>()
    for (const row of items) {
      const ts = new Date(row.bucket).getTime()
      let entry = byBucket.get(ts)
      if (!entry) {
        entry = { bucket: new Date(ts).toISOString() }
        byBucket.set(ts, entry)
      }
      // Empty-key marker rows only establish the bucket on the x-axis.
      if (!row.key) continue
      const slug = slugifyKey(row.key)
      labelBySlug.set(slug, row.key)
      totals.set(slug, (totals.get(slug) ?? 0) + row.request_count)
      entry[slug] = ((entry[slug] as number) ?? 0) + row.request_count
    }

    const keys = Array.from(totals.entries())
      .sort((a, b) => b[1] - a[1])
      .map(([k]) => k)

    const config: ChartConfig = {}
    const colors: Record<string, string> = {}
    keys.forEach((slug, i) => {
      const color = SERIES_COLORS[i % SERIES_COLORS.length]
      config[slug] = { label: labelBySlug.get(slug) ?? slug, color }
      colors[slug] = color
    })

    // Buckets are already gap-filled and ordered by the server; just zero-fill
    // any series missing from a given bucket so the stacked bars render cleanly.
    const rows = Array.from(byBucket.entries())
      .sort((a, b) => a[0] - b[0])
      .map(([, r]) => r)
    for (const r of rows) {
      for (const key of keys) if (r[key] === undefined) r[key] = 0
    }

    return {
      chartData: rows,
      seriesKeys: keys,
      chartConfig: config,
      colorBySlug: colors,
    }
  }, [data])

  const header = (
    <CardHeader>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <CardTitle>AI agents over time</CardTitle>
          <CardDescription>
            {startDate && endDate
              ? `${format(startDate, 'LLL dd, y')} – ${format(endDate, 'LLL dd, y')} · ${bucket} buckets`
              : 'Select a date range'}
          </CardDescription>
        </div>
        <div className="flex items-center gap-1 rounded-md border p-0.5">
          <Button
            size="sm"
            variant={groupBy === 'provider' ? 'default' : 'ghost'}
            className="h-6 px-2 text-xs"
            onClick={() => setGroupBy('provider')}
          >
            By provider
          </Button>
          <Button
            size="sm"
            variant={groupBy === 'agent' ? 'default' : 'ghost'}
            className="h-6 px-2 text-xs"
            onClick={() => setGroupBy('agent')}
          >
            By agent
          </Button>
        </div>
      </div>
    </CardHeader>
  )

  return (
    <Card>
      {header}
      <CardContent>
        {isLoading ? (
          <div className="flex h-[280px] w-full items-center justify-center">
            <div className="text-sm text-muted-foreground">
              Loading chart data…
            </div>
          </div>
        ) : error ? (
          <div className="flex h-[280px] w-full items-center justify-center">
            <div className="text-sm text-destructive">
              Failed to load AI agent timeline
            </div>
          </div>
        ) : !chartData.length ? (
          <div className="flex h-[280px] w-full flex-col items-center justify-center text-center">
            <p className="text-sm text-muted-foreground">
              No AI crawlers hit your site in this period
            </p>
            <p className="mt-1 text-xs text-muted-foreground">
              We watch for OpenAI, Anthropic, Perplexity, Google, Apple, Meta,
              Amazon, ByteDance and more.
            </p>
          </div>
        ) : (
          <ChartContainer config={chartConfig} className="h-[280px] w-full">
            <BarChart
              accessibilityLayer
              data={chartData}
              margin={{ left: 12, right: 12, top: 12, bottom: 12 }}
            >
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="bucket"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                minTickGap={32}
                tickFormatter={(v) => formatBucketTick(String(v), bucket)}
              />
              <YAxis
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                allowDecimals={false}
                tickFormatter={(v) => Number(v).toLocaleString()}
              />
              {seriesKeys.map((key, i) => (
                <Bar
                  key={key}
                  dataKey={key}
                  stackId="a"
                  fill={`var(--color-${key})`}
                  isAnimationActive={false}
                  radius={
                    i === seriesKeys.length - 1 ? [3, 3, 0, 0] : [0, 0, 0, 0]
                  }
                />
              ))}
              <Tooltip
                cursor={{ fill: 'var(--muted)', opacity: 0.4 }}
                wrapperStyle={{ zIndex: 50, outline: 'none' }}
                content={
                  <AiTimelineTooltip
                    bucket={bucket}
                    chartConfig={chartConfig}
                    colorBySlug={colorBySlug}
                  />
                }
              />
            </BarChart>
          </ChartContainer>
        )}
        {/* Custom legend below the chart — wraps + scrolls so a long agent
            taxonomy (~30 entries) stays readable instead of clipping. */}
        {!isLoading && !error && chartData.length > 0 && (
          <div className="mt-3 max-h-20 overflow-y-auto">
            <div className="flex flex-wrap gap-x-3 gap-y-1.5">
              {seriesKeys.map((key) => (
                <div
                  key={key}
                  className="flex items-center gap-1.5 text-xs text-muted-foreground"
                >
                  <span
                    className="size-2 shrink-0 rounded-[2px]"
                    style={{ backgroundColor: colorBySlug[key] }}
                  />
                  <span className="whitespace-nowrap">
                    {chartConfig[key]?.label ?? key}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
