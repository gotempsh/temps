// Shared anomaly-band data for any metric chart.
//
// Given a metric + the aggregation/time-window currently on screen, this finds
// the enabled anomaly rule covering the metric and backtests its detector over
// the visible range (the SAME /otel/alerts/preview the evaluator uses), then
// hands back a `bandSeries` descriptor for ThresholdLineChart plus a `mergeBand`
// that stamps the per-bucket band onto the chart points. Used by the explorer
// drill-in, dashboard tiles, and anywhere a single metric is shown — so the
// expected-range band + breach markers look identical everywhere.
//
// The backtest uses the DISPLAYED aggregation (not the rule's) so the band
// always tracks the line you're looking at, even when you view a different
// aggregation than the rule alerts on.

import { previewAlert, type ProjectResponse } from '@/api/client'
import type { ThresholdBandSeries } from '@/components/charts/threshold-line-chart'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { useAlertStatus } from './alert-status'

interface UseAnomalyBandArgs {
  project: ProjectResponse
  metricName: string
  /** The aggregation token currently displayed (avg|sum|max|pNN|…). */
  aggregation: string
  /** Visible window — ISO strings, memoized upstream to keep the query stable. */
  fromIso: string
  toIso: string
  /** Skip the work entirely (e.g. metric not selected yet). */
  enabled?: boolean
}

export interface AnomalyBand {
  /** Pass to ThresholdLineChart; undefined when there's no band to draw. */
  bandSeries: ThresholdBandSeries | undefined
  /**
   * Stamp the band onto chart points (keyed on each point's `bucket`). A no-op
   * that returns the input unchanged when there's no band. Aligns each chart
   * bucket to the NEAREST backtest bucket, since the rule's window may not match
   * the chart's interval.
   */
  mergeBand: <T extends { bucket: string; value: number }>(data: T[]) => T[]
}

export function useAnomalyBand({
  project,
  metricName,
  aggregation,
  fromIso,
  toIso,
  enabled = true,
}: UseAnomalyBandArgs): AnomalyBand {
  // Reuses the cached listAlerts the status dots already fetch — no extra call.
  const { rules } = useAlertStatus(project.id, { enabled })
  const rule = useMemo(
    () =>
      rules.find(
        (r) =>
          r.enabled &&
          r.metric_name === metricName &&
          r.detection_config.kind === 'anomaly',
      ) ?? null,
    [rules, metricName],
  )

  const query = useQuery({
    queryKey: [
      'anomaly-band',
      project.id,
      metricName,
      aggregation,
      fromIso,
      toIso,
      rule?.id,
      rule?.window_secs,
      JSON.stringify(rule?.detection_config),
    ],
    enabled: enabled && !!rule && metricName.length > 0,
    queryFn: async () => {
      const res = await previewAlert({
        body: {
          project_id: project.id,
          metric_name: metricName,
          aggregation,
          window_secs: rule!.window_secs,
          detection_config: rule!.detection_config,
          start_time: fromIso,
          end_time: toIso,
        },
        throwOnError: true,
      })
      return res.data
    },
  })

  const points = useMemo(() => query.data?.points ?? [], [query.data])
  const sufficient = query.data?.sufficient ?? false

  const bandSeries: ThresholdBandSeries | undefined =
    rule && sufficient && points.length > 0
      ? {
          lowerKey: 'bandLower',
          spanKey: 'bandSpan',
          breachKey: 'bandBreach',
          tone: rule.severity === 'critical' ? 'poor' : 'warn',
        }
      : undefined

  const mergeBand = useMemo(() => {
    const bandTs = points.map((p) => new Date(p.bucket).getTime())
    return <T extends { bucket: string; value: number }>(data: T[]): T[] => {
      if (!bandSeries || points.length === 0) return data
      return data.map((d) => {
        const t = new Date(d.bucket).getTime()
        let best = 0
        let bestDiff = Infinity
        for (let i = 0; i < bandTs.length; i++) {
          const diff = Math.abs(bandTs[i] - t)
          if (diff < bestDiff) {
            bestDiff = diff
            best = i
          }
        }
        const p = points[best]
        return {
          ...d,
          bandLower: p.lower,
          bandSpan: Math.max(0, p.upper - p.lower),
          bandBreach: p.breaching ? d.value : null,
        }
      })
    }
    // bandSeries is derived from points; points identity is stable per fetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [points, bandSeries])

  return { bandSeries, mergeBand }
}
