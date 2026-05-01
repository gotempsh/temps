/**
 * Re-exports for the observability page. The shapes live in the generated
 * SDK; this module exists so call-sites import from one stable path.
 *
 * No `log` kind — runtime stdout/stderr lives on the dedicated Logs page,
 * not on Observe. Volume + storage characteristics make logs unsuitable
 * for the merged business-signal timeline.
 */

export type {
  ErrorRow,
  EventKind,
  EventsResponse,
  ObservabilityEvent,
  RequestRow,
  RevenueRow,
  SpanRow,
} from '@/api/client'

export const ALL_KINDS = [
  'request',
  'span',
  'error',
  'revenue',
] as const satisfies ReadonlyArray<
  import('@/api/client').EventKind
>
