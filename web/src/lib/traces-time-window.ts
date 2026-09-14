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

/**
 * Query time bounds for the Traces list.
 *
 * When pinned to a trace ID, omit the window — same contract as LogsList. An
 * exact ID is already specific, and a freshly ingested span can land after a
 * frozen end_time on a long-lived page.
 */
export function tracesListTimeBounds(
  traceIdSearch: string | undefined,
  window: TracesTimeWindow
): { start_time?: string; end_time?: string } {
  if (traceIdSearch) {
    return { start_time: undefined, end_time: undefined }
  }
  return { start_time: window.startTime, end_time: window.endTime }
}
