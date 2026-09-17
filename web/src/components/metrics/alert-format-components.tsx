// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { STATUS_META, type AlertStatusLevel } from './alert-status'

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
            meta.dotClass
          )}
        />
      )}
      <span
        className={cn(
          'relative inline-flex size-2 rounded-full',
          meta.dotClass
        )}
        aria-hidden
      />
    </span>
  )
}

/** Firing-state badge derived from `rule.last_state` (ok|firing|unknown). */
export function AlertStateBadge({
  state,
  firingSeriesCount,
}: {
  state: string
  /**
   * For a dynamic (per-series) rule with open series, renders "N series
   * firing" instead of a flat "Firing" badge — pass `rule.firing_series.length`
   * only when `rule.dynamic_alerts` is true, so a static rule never shows this.
   */
  firingSeriesCount?: number
}) {
  if (state === 'firing') {
    return (
      <Badge variant="destructive" className="shrink-0">
        {firingSeriesCount ? `${firingSeriesCount} series firing` : 'Firing'}
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
