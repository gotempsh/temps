// "Would this have fired?" backtest for an anomaly alert rule.
//
// Replays the rule's band over the last 7 days via the /otel/alerts/preview
// endpoint (the SAME band the evaluator uses) and shows how often it would have
// fired, plus a chart of the value against its expected band with breach markers.

import { previewAlertMutation } from '@/api/client/@tanstack/react-query.gen'
import type { AnomalyPreviewRequest } from '@/api/client'
import { Skeleton } from '@/components/ui/skeleton'
import { formatBucketLabel } from '@/components/metrics/metric-format'
import { useMutation } from '@tanstack/react-query'
import { AlertTriangle, Loader2 } from 'lucide-react'
import { useEffect, useMemo } from 'react'
import {
  Area,
  CartesianGrid,
  ComposedChart,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'

interface AnomalyBacktestProps {
  projectId: number
  metricName: string
  aggregation: string
  windowSecs: number
  /** The anomaly detector to backtest. */
  detectionConfig: AnomalyPreviewRequest['detection_config']
}

function fmtNum(v: number): string {
  if (!Number.isFinite(v)) return '—'
  const abs = Math.abs(v)
  if (abs >= 1000) return v.toLocaleString(undefined, { maximumFractionDigits: 0 })
  if (abs >= 1) return v.toFixed(1)
  return v.toFixed(3)
}

export function AnomalyBacktest({
  projectId,
  metricName,
  aggregation,
  windowSecs,
  detectionConfig,
}: AnomalyBacktestProps) {
  const preview = useMutation({
    ...previewAlertMutation(),
    meta: { errorTitle: 'Backtest failed' },
  })

  // Re-run (debounced) whenever the rule's inputs change. The body is stable
  // JSON so the debounce key only changes on a real edit.
  const body = useMemo(
    () => ({
      project_id: projectId,
      metric_name: metricName,
      aggregation,
      window_secs: windowSecs,
      detection_config: detectionConfig,
    }),
    [projectId, metricName, aggregation, windowSecs, detectionConfig],
  )
  const bodyKey = JSON.stringify(body)
  const { mutate } = preview
  useEffect(() => {
    if (!metricName || !Number.isFinite(windowSecs) || windowSecs <= 0) return
    const t = setTimeout(() => mutate({ body }), 500)
    return () => clearTimeout(t)
    // bodyKey captures every meaningful input; `body`/`mutate` are stable refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bodyKey])

  const chartData = useMemo(() => {
    const points = preview.data?.points ?? []
    return points.map((p) => ({
      bucket: p.bucket,
      value: p.value,
      lower: p.lower,
      band: Math.max(0, p.upper - p.lower),
      breach: p.breaching ? p.value : null,
    }))
  }, [preview.data])

  // A raw breach COUNT reads the same whether it's one sustained regime shift
  // (the metric stepped to a new level and never came back) or dozens of
  // separate spikes — those call for very different fixes (widen the band /
  // add seasonality vs. the rule is working as intended). Count contiguous
  // breaching runs so the copy can tell them apart.
  const episodeCount = useMemo(() => {
    const points = preview.data?.points ?? []
    let episodes = 0
    let wasBreaching = false
    for (const p of points) {
      if (p.breaching && !wasBreaching) episodes += 1
      wasBreaching = p.breaching
    }
    return episodes
  }, [preview.data])

  const header = (
    <div className="flex items-center gap-2 text-sm font-medium">
      Backtest — last 7 days
      {preview.isPending && (
        <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
      )}
    </div>
  )

  if (preview.isError) {
    return (
      <div className="rounded-lg border border-border/60 p-3">
        {header}
        <p className="mt-1 flex items-center gap-1.5 text-xs text-amber-600 dark:text-amber-400">
          <AlertTriangle className="size-3.5" />
          Couldn&apos;t run the backtest for this metric.
        </p>
      </div>
    )
  }

  if (!preview.data) {
    return (
      <div className="rounded-lg border border-border/60 p-3">
        {header}
        <Skeleton className="mt-2 h-[140px] w-full" />
      </div>
    )
  }

  const { breach_count, sufficient } = preview.data

  return (
    <div className="rounded-lg border border-border/60 p-3">
      {header}
      {!sufficient ? (
        <p className="mt-1 flex items-center gap-1.5 text-xs text-amber-600 dark:text-amber-400">
          <AlertTriangle className="size-3.5" />
          Not enough history to backtest yet — the band needs more data.
        </p>
      ) : (
        <p className="mt-1 text-xs text-muted-foreground">
          {breach_count === 0 ? (
            <>
              Would <span className="font-medium text-emerald-600">not</span>{' '}
              have fired — no points left the band.
            </>
          ) : episodeCount === 1 ? (
            <>
              Would have fired{' '}
              <span className="font-medium text-foreground">once</span> and
              stayed breaching for{' '}
              <span className="font-medium text-foreground">
                {breach_count}
              </span>{' '}
              of the {chartData.length} evaluated points — the metric shifted
              to a new level and never returned to the band, not a series of
              separate spikes.
            </>
          ) : (
            <>
              Would have fired{' '}
              <span className="font-medium text-foreground">
                {episodeCount} separate times
              </span>{' '}
              ({breach_count} of {chartData.length} points breaching).
            </>
          )}
        </p>
      )}

      {chartData.length > 0 && (
        <div className="mt-2 h-[160px] w-full">
          <ResponsiveContainer width="100%" height="100%">
            <ComposedChart
              data={chartData}
              margin={{ top: 4, right: 8, bottom: 0, left: 0 }}
            >
              <CartesianGrid strokeDasharray="3 3" className="stroke-border/40" />
              <XAxis
                dataKey="bucket"
                tickFormatter={formatBucketLabel}
                tick={{ fontSize: 10 }}
                tickLine={false}
                axisLine={false}
                minTickGap={48}
              />
              <YAxis
                tickFormatter={fmtNum}
                tick={{ fontSize: 10 }}
                tickLine={false}
                axisLine={false}
                width={44}
              />
              {/* Expected band: invisible base at `lower`, shaded `band` on top. */}
              <Area
                dataKey="lower"
                stackId="band"
                stroke="none"
                fill="none"
                isAnimationActive={false}
              />
              <Area
                dataKey="band"
                stackId="band"
                stroke="none"
                fill="var(--chart-1)"
                fillOpacity={0.12}
                isAnimationActive={false}
              />
              <Line
                dataKey="value"
                stroke="var(--chart-1)"
                strokeWidth={1.5}
                dot={false}
                isAnimationActive={false}
              />
              {/* Breach overlay: same stroke width as the value line so a
                  long breaching run reads as "the line turned red", not a
                  thick blob of overlapping dots (the previous Scatter marker
                  visually dominated the chart once more than a few adjacent
                  points breached). Nulls on non-breaching points leave gaps. */}
              <Line
                dataKey="breach"
                stroke="var(--destructive)"
                strokeWidth={1.5}
                dot={false}
                connectNulls={false}
                isAnimationActive={false}
              />
              <Tooltip
                contentStyle={{ fontSize: 11 }}
                labelFormatter={(l) => formatBucketLabel(String(l))}
                formatter={(v, name) =>
                  name === 'band' ? null : [fmtNum(Number(v)), name]
                }
              />
            </ComposedChart>
          </ResponsiveContainer>
        </div>
      )}
    </div>
  )
}
