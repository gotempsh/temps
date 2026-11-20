// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** What a trace detector recorded on a performance issue's event. */
export interface PerformanceEvidence {
  detector: string
  traceId: string
  spanIds: string[]
  occurrences: number
  durationMs: number
  pattern: string
  operation: string
  service: string
}

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

/**
 * The evidence of a performance issue from a stored event's data
 * (`trace.extra.performance`), or `null` for an ordinary error event.
 */
export function performanceEvidence(data: unknown): PerformanceEvidence | null {
  const perf = record(record(record(record(data)?.trace)?.extra)?.performance)
  if (!perf || typeof perf.trace_id !== 'string') return null
  const text = (key: string) =>
    typeof perf[key] === 'string' ? (perf[key] as string) : ''
  const num = (key: string) =>
    typeof perf[key] === 'number' ? (perf[key] as number) : 0
  return {
    detector: text('detector'),
    traceId: perf.trace_id,
    spanIds: Array.isArray(perf.span_ids)
      ? perf.span_ids.filter((id): id is string => typeof id === 'string')
      : [],
    occurrences: num('occurrences'),
    durationMs: num('duration_ms'),
    pattern: text('pattern'),
    operation: text('operation'),
    service: text('service'),
  }
}

/** What each detector means, for the issue page. */
export const DETECTOR_ADVICE: Record<string, string> = {
  n_plus_one_query:
    'The same query ran once per item. Load the items in one query (a join, or WHERE id IN (...)) instead of one query per item.',
  slow_db_query:
    'One query took over a second. Check its plan for a missing index, or narrow what it reads.',
  sequential_http:
    'These calls waited for each other but do not depend on each other. Running them concurrently would cut the wait to the slowest one.',
  oversized_payload:
    'A body over 1 MB was sent. Paginate, compress, or return only the fields the caller needs.',
}
