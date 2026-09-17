// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@temps-sdk/ui'
import { cn } from './lib/cn'

/**
 * The console's whole status vocabulary. Generalizes the shape already used
 * by `AlertStateBadge`/`StatusDot` (web/src/components/metrics/alert-format.tsx)
 * and `STATUS_META` (alert-status.ts) beyond alerting to every tone-driven
 * state in the app — deployments, services, backups, nodes. Five tones only;
 * do not add a sixth without checking every Badge variant="..." call site
 * first (variants map 1:1 onto `--success`/`--warning`/`--destructive`).
 */
export type StatusTone = 'ok' | 'warn' | 'error' | 'idle' | 'running'

interface StatusToneMeta {
  label: string
  dotClass: string
  badgeVariant: 'success' | 'warning' | 'destructive' | 'secondary' | 'default'
  pulse?: boolean
}

export const STATUS_TONES: Record<StatusTone, StatusToneMeta> = {
  ok: { label: 'OK', dotClass: 'bg-success', badgeVariant: 'success' },
  warn: { label: 'Warn', dotClass: 'bg-warning', badgeVariant: 'warning' },
  error: {
    label: 'Error',
    dotClass: 'bg-destructive',
    badgeVariant: 'destructive',
    pulse: true,
  },
  idle: { label: 'Idle', dotClass: 'bg-muted-foreground', badgeVariant: 'secondary' },
  running: {
    label: 'Running',
    dotClass: 'bg-primary',
    badgeVariant: 'default',
    pulse: true,
  },
}

export function StatusDot({
  tone,
  className,
}: {
  tone: StatusTone
  className?: string
}) {
  const meta = STATUS_TONES[tone]
  return (
    <span className={cn('relative inline-flex size-2 shrink-0', className)}>
      {meta.pulse ? (
        <span
          className={cn(
            'absolute inline-flex size-full animate-ping rounded-full opacity-60',
            meta.dotClass,
          )}
        />
      ) : null}
      <span
        className={cn('relative inline-flex size-2 rounded-full', meta.dotClass)}
        aria-hidden
      />
    </span>
  )
}

export interface StatusProps {
  tone: StatusTone
  /** Overrides the tone's default word (e.g. "3 series firing" instead of "Error"). */
  label?: string
  /** Renders dot + badge (default) or just the dot, for dense table rows. */
  variant?: 'badge' | 'dot'
  className?: string
}

/**
 * The one status primitive: a tone-driven dot + word, in a `Badge` by
 * default. Color never stands alone — every render carries the word too, so
 * meaning survives colorblindness and B/W printing.
 */
export function Status({ tone, label, variant = 'badge', className }: StatusProps) {
  const meta = STATUS_TONES[tone]
  const text = label ?? meta.label
  if (variant === 'dot') {
    return (
      <span className={cn('inline-flex items-center gap-1.5 text-sm', className)}>
        <StatusDot tone={tone} />
        {text}
      </span>
    )
  }
  return (
    <Badge variant={meta.badgeVariant} className={cn('gap-1.5', className)}>
      <StatusDot tone={tone} />
      {text}
    </Badge>
  )
}
