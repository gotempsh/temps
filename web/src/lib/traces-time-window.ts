// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { resolveTimeRange } from './time-range-filter'

export type TracesTimeRange = string

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
  now: Date = new Date()
): TracesTimeWindow {
  const range = resolveTimeRange(timeRange, now.getTime())
  return { startTime: range.from, endTime: range.to }
}

/** Keep timestamp partition pruning even when filtering by an exact trace ID. */
export function tracesListTimeBounds(
  _traceIdSearch: string | undefined,
  window: TracesTimeWindow
): { start_time: string; end_time: string } {
  return { start_time: window.startTime, end_time: window.endTime }
}

export type TraceTimeBounds = { start_time?: string; end_time?: string }

/** Include the whole trace plus padding for clock skew and late child spans. */
export function traceTimeBounds(trace: {
  start_time: string
  duration_ms: number
}): TraceTimeBounds {
  const start = Date.parse(trace.start_time)
  const duration = trace.duration_ms
  if (!Number.isFinite(start) || !Number.isFinite(duration) || duration < 0) {
    return {}
  }
  const padding = 5 * 60_000
  const end = start + duration + padding
  if (duration + 2 * padding > 31 * 24 * 3600_000 || !Number.isFinite(end)) {
    return {}
  }
  return {
    start_time: new Date(start - padding).toISOString(),
    end_time: new Date(end).toISOString(),
  }
}

/** Preserve supplied bounds so the API can report invalid links explicitly. */
export function traceTimeBoundsFromSearch(
  params: URLSearchParams
): TraceTimeBounds {
  return {
    start_time: params.get('start_time') ?? undefined,
    end_time: params.get('end_time') ?? undefined,
  }
}

export function traceDetailPath(trace: {
  trace_id: string
  start_time: string
  duration_ms: number
}): string {
  const bounds = traceTimeBounds(trace)
  const params = new URLSearchParams()
  if (bounds.start_time) params.set('start_time', bounds.start_time)
  if (bounds.end_time) params.set('end_time', bounds.end_time)
  const search = params.toString()
  return `${encodeURIComponent(trace.trace_id)}${search ? `?${search}` : ''}`
}
