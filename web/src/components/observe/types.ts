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

/**
 * Metric kind for the unified Observe stream — SCAFFOLDED, OFF BY DEFAULT.
 *
 * The dedicated OTel metrics explorer lives at `metrics/*` (see
 * `pages/MetricsExplorer.tsx`). Surfacing individual metric datapoints inside
 * the merged Observe timeline additionally requires the backend events
 * endpoint to emit a `metric` variant: a new `EventKind::Metric` plus a
 * `MetricRow` arm on `ObservabilityEvent` in the merged events handler. That
 * is backend work outside Phases A–C, so the generated SDK's `EventKind`
 * union (`'request' | 'span' | 'error' | 'revenue'`) does NOT yet include
 * `'metric'`.
 *
 * Intent is recorded here so the wiring is a single, well-marked step once the
 * SDK is regenerated:
 *   1. Backend: add `EventKind::Metric` + `MetricRow` to the events response
 *      and the merged query.
 *   2. cd web && bun run openapi-ts   # regenerates `EventKind`/`ObservabilityEvent`.
 *   3. Append `'metric'` to `ALL_KINDS` above (the `satisfies` constraint will
 *      then accept it), add a `KIND_META` entry + `consoleSummary` arm in
 *      `pages/Observe.tsx`, and — because metrics are high-volume — leave it
 *      OUT of `DEFAULT_KINDS` so it stays off by default like Traces.
 *
 * `METRIC_KIND` is the literal to append; kept as a const so the future change
 * is grep-able and the default-off policy is documented next to it.
 */
export const METRIC_KIND = 'metric' as const

/** Whether the metric kind is loaded on first Observe visit. Off — metric
 *  datapoints are high-cardinality/high-volume, same rationale as Traces. */
export const METRIC_KIND_DEFAULT_ON = false
