// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { StatusDot } from './alert-format'
import type { AlertStatusLevel, StatusRollup } from './alert-status'

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
          className
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
        className
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
