// Shared formatting helpers for OTel metric alert rules.
//
// Keeps the list rows, the form header, and any future surfaces rendering the
// firing-state badge + one-line rule summary identically.

import type { OtelMetricAlertRuleResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { aggregationLabel } from './metric-format'
import { STATUS_META, type AlertStatusLevel } from './alert-status'

/** Comparator tokens the backend accepts. */
export const COMPARATORS: { value: string; label: string; symbol: string }[] = [
  { value: 'gt', label: 'Greater than (>)', symbol: '>' },
  { value: 'gte', label: 'Greater or equal (≥)', symbol: '≥' },
  { value: 'lt', label: 'Less than (<)', symbol: '<' },
  { value: 'lte', label: 'Less or equal (≤)', symbol: '≤' },
]

export const SEVERITIES: { value: string; label: string }[] = [
  { value: 'info', label: 'Info' },
  { value: 'warning', label: 'Warning' },
  { value: 'critical', label: 'Critical' },
]

/** Detector kinds the form can author. Only static + anomaly are evaluable. */
export const DETECTION_KINDS: { value: 'static' | 'anomaly'; label: string }[] =
  [
    { value: 'static', label: 'Static threshold' },
    { value: 'anomaly', label: 'Anomaly (auto baseline)' },
  ]

/** Anomaly baseline algorithms accepted today (agile/ewma are rejected v1). */
export const ANOMALY_ALGORITHMS: { value: string; label: string }[] = [
  { value: 'robust', label: 'Robust (seasonal, stable)' },
  { value: 'basic', label: 'Basic (non-seasonal)' },
]

export const DIRECTIONS: { value: string; label: string }[] = [
  { value: 'both', label: 'Above or below' },
  { value: 'above', label: 'Above only' },
  { value: 'below', label: 'Below only' },
]

export const SEASONALITIES: { value: string; label: string }[] = [
  { value: 'none', label: 'None' },
  { value: 'hourly', label: 'Hourly' },
  { value: 'daily', label: 'Daily' },
  { value: 'weekly', label: 'Weekly' },
]

/**
 * Friendly sensitivity presets that map to the raw `deviations` (σ) the backend
 * expects. Lower σ = a tighter band = more alerts, so "High sensitivity" is the
 * lowest σ. The raw number stays available under "Custom".
 */
export const SENSITIVITY_PRESETS: {
  value: string
  label: string
  deviations: number
}[] = [
  { value: 'high', label: 'High — more alerts (2σ)', deviations: 2 },
  { value: 'medium', label: 'Medium — balanced (3σ)', deviations: 3 },
  { value: 'low', label: 'Low — fewer alerts (4σ)', deviations: 4 },
]

/** The preset key matching a σ value, or `'custom'` if it's not a preset. */
export function presetForDeviations(deviations: number): string {
  return (
    SENSITIVITY_PRESETS.find((p) => p.deviations === deviations)?.value ??
    'custom'
  )
}

/** Roughly how many days of history an anomaly baseline wants before it alerts. */
export const ANOMALY_MIN_HISTORY_DAYS = 14

export function comparatorSymbol(comparator: string): string {
  return COMPARATORS.find((c) => c.value === comparator)?.symbol ?? comparator
}

/**
 * Compact one-line description of a rule for the list row, e.g.
 * `avg http.server.duration > 500 · warning`.
 *
 * Reads the detector from the typed `detection_config` discriminated union.
 * Today only `static` rules exist; the fallback covers future kinds
 * (anomaly/forecast/outlier/auto_watch) without breaking the row.
 */
export function alertSummary(rule: OtelMetricAlertRuleResponse): string {
  const agg = aggregationLabel(rule.aggregation)
  const cfg = rule.detection_config
  if (cfg.kind === 'static') {
    return `${agg} ${rule.metric_name} ${comparatorSymbol(cfg.comparator)} ${cfg.threshold} · ${rule.severity}`
  }
  if (cfg.kind === 'anomaly') {
    // e.g. `avg http.server.duration · anomaly ±3σ · warning`
    return `${agg} ${rule.metric_name} · anomaly ±${cfg.deviations}σ · ${rule.severity}`
  }
  return `${agg} ${rule.metric_name} · ${cfg.kind} · ${rule.severity}`
}

/**
 * A small colored status dot (Datadog's status language). The dot is decorative
 * — callers must pair it with text/badge so meaning isn't conveyed by hue alone.
 * `pulse` adds a quiet ping for live `alert` states.
 */
export function StatusDot({
  level,
  pulse = false,
  className,
  title,
}: {
  level: AlertStatusLevel
  pulse?: boolean
  className?: string
  title?: string
}) {
  const meta = STATUS_META[level]
  return (
    <span
      className={cn('relative inline-flex size-2 shrink-0', className)}
      title={title}
    >
      {pulse && level === 'alert' && (
        <span
          className={cn(
            'absolute inline-flex size-full animate-ping rounded-full opacity-60',
            meta.dotClass,
          )}
        />
      )}
      <span
        className={cn(
          'relative inline-flex size-2 rounded-full',
          meta.dotClass,
        )}
        aria-hidden
      />
    </span>
  )
}

/** Firing-state badge derived from `rule.last_state` (ok|firing|unknown). */
export function AlertStateBadge({ state }: { state: string }) {
  if (state === 'firing') {
    return (
      <Badge variant="destructive" className="shrink-0">
        Firing
      </Badge>
    )
  }
  if (state === 'ok') {
    return (
      <Badge variant="success" className="shrink-0">
        OK
      </Badge>
    )
  }
  return (
    <Badge
      variant="secondary"
      className="shrink-0"
      title="Not evaluated yet. Anomaly rules stay 'unknown' until the metric has enough history to build a baseline."
    >
      Unknown
    </Badge>
  )
}
