// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Circle,
  CircleCheck,
  CircleX,
  LoaderCircle,
  TriangleAlert,
  type LucideIcon,
} from 'lucide-react'
import { Badge } from '@temps-sdk/ui'
import { cn } from './lib/cn'

/**
 * The console's whole status vocabulary. Generalizes the shape already used
 * by `AlertStateBadge`/`StatusDot` (web/src/components/metrics/alert-format.tsx)
 * and `STATUS_META` (alert-status.ts) beyond alerting to every tone-driven
 * state in the app — deployments, services, backups, nodes. Five tones only;
 * do not add a sixth without checking every Badge variant="..." call site
 * first (variants map 1:1 onto `--success`/`--warning`/`--destructive`).
 *
 * Each tone pairs a color with a shape (an icon), never color alone — a
 * small dot still exists for the compact `variant="dot"` row form where a
 * full icon would be too heavy, but the default badge form always shows the
 * icon so meaning survives colorblindness at a glance, not just via the word.
 */
export type StatusTone = 'ok' | 'warn' | 'error' | 'idle' | 'running'

interface StatusToneMeta {
  label: string
  dotClass: string
  icon: LucideIcon
  badgeVariant: 'success' | 'warning' | 'destructive' | 'secondary' | 'default'
  pulse?: boolean
  spin?: boolean
}

export const STATUS_TONES: Record<StatusTone, StatusToneMeta> = {
  ok: {
    label: 'OK',
    dotClass: 'bg-success',
    icon: CircleCheck,
    badgeVariant: 'success',
  },
  warn: {
    label: 'Warn',
    dotClass: 'bg-warning',
    icon: TriangleAlert,
    badgeVariant: 'warning',
  },
  error: {
    label: 'Error',
    dotClass: 'bg-destructive',
    icon: CircleX,
    badgeVariant: 'destructive',
    pulse: true,
  },
  idle: {
    label: 'Idle',
    dotClass: 'bg-muted-foreground',
    icon: Circle,
    badgeVariant: 'secondary',
  },
  running: {
    label: 'Running',
    dotClass: 'bg-primary',
    icon: LoaderCircle,
    badgeVariant: 'default',
    pulse: true,
    spin: true,
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
  /** Renders icon + badge (default) or a compact dot + word, for dense table rows. */
  variant?: 'badge' | 'dot'
  className?: string
}

/**
 * The one status primitive: a tone-driven icon (or, in `variant="dot"`, a
 * small dot) + word, in a `Badge` by default. Color never stands alone —
 * shape and word both carry the meaning too, so it survives colorblindness
 * and B/W printing.
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
  const Icon = meta.icon
  return (
    <Badge variant={meta.badgeVariant} className={cn('gap-1.5', className)}>
      <Icon className={cn('size-3.5', meta.spin && 'animate-spin')} aria-hidden />
      {text}
    </Badge>
  )
}
