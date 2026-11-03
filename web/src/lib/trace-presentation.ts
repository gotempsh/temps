// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Compact display only: sorting continues to use the original millisecond value. */
export function formatTraceDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—'
  if (ms === 0) return '0 ms'
  if (ms < 0.001) return '<1 µs'
  if (ms < 1) return `${Math.round(ms * 1000)} µs`
  if (ms < 1000) return `${Math.round(ms)} ms`
  return `${Number((ms / 1000).toFixed(1))} s`
}

export type TraceOutcome = 'error' | 'ok' | 'unset'

/**
 * A trace failed if any span errored or the root says so. `UNSET` is the
 * OpenTelemetry default, not a failure: the instrumentation set no status.
 */
export function traceOutcome(trace: {
  error_count: number
  status_code?: string | null
}): TraceOutcome {
  if (trace.error_count > 0 || trace.status_code === 'ERROR') return 'error'
  return trace.status_code === 'OK' ? 'ok' : 'unset'
}

/**
 * How long a duration bar is drawn, as a share of the slowest trace on the
 * page. Any non-zero duration keeps a sliver, so it never reads as empty.
 */
export function durationShare(ms: number, slowestMs: number): number {
  if (!Number.isFinite(ms) || ms <= 0) return 0
  if (!Number.isFinite(slowestMs) || slowestMs <= 0) return 0
  return Math.min(1, Math.max(0.04, ms / slowestMs))
}

/** Latency band of a duration bar: fast, slow-ish, slow, very slow. */
export function durationBarClass(ms: number): string {
  if (ms < 100) return 'bg-emerald-500'
  if (ms < 500) return 'bg-amber-500'
  if (ms < 2000) return 'bg-orange-500'
  return 'bg-red-500'
}
