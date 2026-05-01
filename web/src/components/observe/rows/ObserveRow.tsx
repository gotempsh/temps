import {
  AlertOctagon,
  CircleDollarSign,
  Network,
  Workflow,
} from 'lucide-react'
import type { ObservabilityEvent } from '../types'
import { ObserveRowShell, SeverityBadge, StatusBadge } from './RowParts'

/**
 * Single entry point: switches on `event.type` to pick the right renderer.
 * Rows render entirely from the row payload — no follow-up fetch.
 */
export function ObserveRow({
  event,
  onClick,
}: {
  event: ObservabilityEvent
  onClick: () => void
}) {
  switch (event.type) {
    case 'request':
      return (
        <ObserveRowShell
          ts={event.ts}
          icon={<Network className="h-3.5 w-3.5" />}
          primary={
            <>
              <span className="font-mono text-xs">{event.method}</span>{' '}
              <span className="truncate">{event.path}</span>
            </>
          }
          secondary={event.host}
          meta={
            <>
              <StatusBadge status={event.status} />
              {event.latency_ms != null && (
                <span className="font-mono tabular-nums">
                  {event.latency_ms}ms
                </span>
              )}
            </>
          }
          onClick={onClick}
        />
      )
    case 'span':
      return (
        <ObserveRowShell
          ts={event.ts}
          icon={<Workflow className="h-3.5 w-3.5" />}
          primary={event.operation}
          secondary={event.service}
          meta={
            event.duration_ms != null ? (
              <span className="font-mono tabular-nums">
                {event.duration_ms.toFixed(1)}ms
              </span>
            ) : null
          }
          onClick={onClick}
        />
      )
    case 'error':
      return (
        <ObserveRowShell
          ts={event.ts}
          icon={<AlertOctagon className="h-3.5 w-3.5 text-destructive" />}
          primary={event.error_class}
          secondary={event.message ?? undefined}
          meta={<SeverityBadge severity="error" />}
          onClick={onClick}
        />
      )
    case 'revenue':
      return (
        <ObserveRowShell
          ts={event.ts}
          icon={<CircleDollarSign className="h-3.5 w-3.5 text-emerald-500" />}
          primary={event.event_type}
          secondary={event.customer_ref ?? undefined}
          meta={
            event.amount_minor != null ? (
              <span className="font-mono tabular-nums text-emerald-600 dark:text-emerald-400">
                {formatAmount(event.amount_minor, event.currency)}
              </span>
            ) : null
          }
          onClick={onClick}
        />
      )
  }
}

function formatAmount(
  minor: number,
  currency: string | null | undefined,
): string {
  const major = minor / 100
  if (!currency) return major.toFixed(2)
  try {
    return new Intl.NumberFormat(undefined, {
      style: 'currency',
      currency: currency.toUpperCase(),
    }).format(major)
  } catch {
    return `${major.toFixed(2)} ${currency.toUpperCase()}`
  }
}
