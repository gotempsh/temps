// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { CompactRow } from '@temps-sdk/ds'

/**
 * One-line tabular row used by every Observe event renderer. Promoted into
 * @temps-sdk/ds as `CompactRow` (generalized beyond Observe) — this is now a
 * thin wrapper keeping the `ts` prop name Observe's call sites already use.
 * `StatusBadge`/`SeverityBadge` below stay Observe-specific: they classify
 * HTTP status codes and log severities, which don't map cleanly onto
 * `Status`'s five-tone health vocabulary (a 2xx/3xx/4xx/5xx code isn't a
 * health verdict, and "info"/"debug" severities have no equivalent tone).
 */
export function ObserveRowShell({
  ts,
  icon,
  primary,
  secondary,
  meta,
  onClick,
}: {
  ts: string
  icon: React.ReactNode
  primary: React.ReactNode
  secondary?: React.ReactNode
  meta?: React.ReactNode
  onClick?: () => void
}) {
  return (
    <CompactRow
      timestamp={ts}
      icon={icon}
      primary={primary}
      secondary={secondary}
      meta={meta}
      onClick={onClick}
    />
  )
}

export function StatusBadge({ status }: { status: number }) {
  const variant: 'default' | 'destructive' | 'secondary' | 'outline' =
    status >= 500
      ? 'destructive'
      : status >= 400
        ? 'secondary'
        : status >= 300
          ? 'outline'
          : 'default'
  return (
    <Badge variant={variant} className="font-mono tabular-nums">
      {status}
    </Badge>
  )
}

export function SeverityBadge({ severity }: { severity: string }) {
  const lower = severity.toLowerCase()
  const variant: 'default' | 'destructive' | 'secondary' | 'outline' =
    lower === 'error' || lower === 'fatal'
      ? 'destructive'
      : lower === 'warn' || lower === 'warning'
        ? 'secondary'
        : 'outline'
  return (
    <Badge variant={variant} className="uppercase tracking-wide">
      {severity}
    </Badge>
  )
}
