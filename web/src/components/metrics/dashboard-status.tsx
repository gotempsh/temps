// Datadog-style firing status for metric dashboards.
//
// A dashboard's status is the worst monitor status across the metrics its tiles
// plot — so a glance at the list tells you which dashboard has a problem, and
// the dashboard header summarises "what's on fire" without opening every tile.
// All of it is derived from the same cached alert-rule fetch the tiles already
// use (useAlertStatus), so there's no extra round-trip and the signal can never
// disagree with the per-tile dots.

import type { OtelDashboardResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { StatusDot } from './alert-format'
import type { AlertStatusLevel, StatusRollup } from './alert-status'

type Section = NonNullable<OtelDashboardResponse['layout']>['sections'][number]

/** Flatten a dashboard's sections to the (metric, aggregation) pairs it plots. */
export function dashboardTiles(
  sections: Section[] | undefined,
): { metricName: string; aggregation?: string }[] {
  return (sections ?? []).flatMap((s) =>
    (s.tiles ?? []).map((t) => ({
      metricName: t.metric_name,
      aggregation: t.aggregation,
    })),
  )
}

/**
 * Dashboard status pill for the view header. Shows a firing count when a monitor
 * on this dashboard is firing, a quiet "All clear" only when tiles are actually
 * watched, and nothing when no rule covers the dashboard (honest — no vanity
 * green on a dashboard nobody is alerting on).
 */
export function DashboardStatusBadge({
  rollup,
  className,
}: {
  rollup: StatusRollup
  className?: string
}) {
  if (rollup.firing > 0 && rollup.level) {
    return (
      <Badge
        variant={rollup.level === 'alert' ? 'destructive' : 'warning'}
        className={cn('shrink-0', className)}
        title={`${rollup.counts.alert} alerting, ${rollup.counts.warn} warning`}
      >
        {rollup.firing} firing
      </Badge>
    )
  }
  if (rollup.watched > 0) {
    return (
      <span
        className={cn(
          'inline-flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground',
          className,
        )}
        title={`${rollup.watched} metric${rollup.watched === 1 ? '' : 's'} watched, none firing`}
      >
        <StatusDot level="ok" />
        All clear
      </span>
    )
  }
  return null
}

/**
 * Compact inline "N firing" count for a section header / list row summary.
 * Renders nothing unless something is firing, so it only ever draws the eye to
 * trouble.
 */
export function FiringCount({
  rollup,
  className,
}: {
  rollup: StatusRollup
  className?: string
}) {
  if (rollup.firing === 0 || !rollup.level) return null
  const tone: Record<Exclude<AlertStatusLevel, 'nodata' | 'ok'>, string> = {
    alert: 'text-destructive',
    warn: 'text-warning',
  }
  return (
    <span
      className={cn(
        'font-medium',
        rollup.level === 'alert' ? tone.alert : tone.warn,
        className,
      )}
      // Severity must not rely on hue alone — spell it out for hover / AT.
      title={
        rollup.level === 'alert'
          ? `${rollup.counts.alert} critical${rollup.counts.warn ? `, ${rollup.counts.warn} warning` : ''} firing`
          : `${rollup.counts.warn} warning firing`
      }
    >
      {rollup.firing} firing
    </span>
  )
}
