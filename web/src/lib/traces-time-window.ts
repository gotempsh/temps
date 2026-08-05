export type TracesTimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

export type TracesTimeWindow = {
  startTime: string
  endTime: string
}

/**
 * Relative time window for the Traces list.
 *
 * Callers that keep a page open must recompute against a fresh `now` (e.g. on
 * Refresh) — freezing the window at mount filters out newly ingested spans.
 */
export function computeTracesTimeWindow(
  timeRange: TracesTimeRange,
  now: Date = new Date(),
): TracesTimeWindow {
  const start = new Date(now.getTime())
  switch (timeRange) {
    case '1h':
      start.setHours(start.getHours() - 1)
      break
    case '6h':
      start.setHours(start.getHours() - 6)
      break
    case '24h':
      start.setDate(start.getDate() - 1)
      break
    case '7d':
      start.setDate(start.getDate() - 7)
      break
    case '30d':
      start.setDate(start.getDate() - 30)
      break
  }
  return { startTime: start.toISOString(), endTime: now.toISOString() }
}

/**
 * Query time bounds for the Traces list.
 *
 * When pinned to a trace ID, omit the window — same contract as LogsList. An
 * exact ID is already specific, and a freshly ingested span can land after a
 * frozen end_time on a long-lived page.
 */
export function tracesListTimeBounds(
  traceIdSearch: string | undefined,
  window: TracesTimeWindow,
): { start_time?: string; end_time?: string } {
  if (traceIdSearch) {
    return { start_time: undefined, end_time: undefined }
  }
  return { start_time: window.startTime, end_time: window.endTime }
}
