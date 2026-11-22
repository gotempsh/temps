// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Gauges Temps publishes from SDK sessions every five minutes (ADR 111, D3). */
export const CRASH_FREE_SESSIONS_METRIC = 'release.crash_free_sessions'
export const CRASH_FREE_USERS_METRIC = 'release.crash_free_users'

/** The `preset` search parameter that opens a prefilled metric alert. */
export const CRASH_FREE_ALERT_PRESET = 'crash-free-sessions'

/** Default crash-free threshold for a new alert: alert below 99%. */
export const DEFAULT_CRASH_FREE_THRESHOLD = 0.99

export const RELEASE_HEALTH_DAYS = [1, 7, 14] as const

/** Days from a URL parameter, falling back to the 7-day default. */
export function healthDaysFromParam(value: string | null): number {
  const days = Number(value)
  return (RELEASE_HEALTH_DAYS as readonly number[]).includes(days) ? days : 7
}

/** A rate as a percentage with enough digits to tell 99.9% from 100%. */
export function formatRate(rate: number | null | undefined): string {
  if (rate == null || !Number.isFinite(rate)) return '—'
  if (rate >= 1) return '100%'
  const percent = rate * 100
  // Never round a rate below 100% up to "100%".
  const digits = percent >= 99 ? 2 : 1
  const factor = 10 ** digits
  return `${(Math.floor(percent * factor) / factor).toFixed(digits)}%`
}

export type RateTone = 'healthy' | 'warning' | 'critical' | 'unknown'

/** How a crash-free rate should read: under 98% is critical, under 99.5% a warning. */
export function rateTone(rate: number | null | undefined): RateTone {
  if (rate == null || !Number.isFinite(rate)) return 'unknown'
  if (rate < 0.98) return 'critical'
  if (rate < 0.995) return 'warning'
  return 'healthy'
}

/** A commit SHA release shortened to 12 characters; other names unchanged. */
export function releaseLabel(release: string): string {
  return /^[0-9a-f]{40}$/i.test(release) ? release.slice(0, 12) : release
}

/** Link to the metric alert form, prefilled with a crash-free drop rule. */
export function crashFreeAlertHref(projectSlug: string): string {
  return `/projects/${projectSlug}/metrics/alerts/new?preset=${CRASH_FREE_ALERT_PRESET}`
}

export interface CrashFreeAlertPreset {
  name: string
  metric_name: string
  aggregation: 'min'
  detection_kind: 'static'
  comparator: 'lt'
  threshold: number
  window_secs: number
  for_duration_secs: number
  group_by: string[]
  dynamic_alerts: boolean
}

/**
 * Form values for a crash-free drop alert when the URL asks for one: the
 * lowest published rate over 15 minutes, below 99% for 10 minutes, one
 * alert per release. `null` for any other URL.
 */
export function alertPresetFromParams(
  params: URLSearchParams
): CrashFreeAlertPreset | null {
  if (params.get('preset') !== CRASH_FREE_ALERT_PRESET) return null
  return {
    name: 'Crash-free sessions dropped',
    metric_name: CRASH_FREE_SESSIONS_METRIC,
    aggregation: 'min',
    detection_kind: 'static',
    comparator: 'lt',
    threshold: DEFAULT_CRASH_FREE_THRESHOLD,
    window_secs: 900,
    for_duration_secs: 600,
    group_by: ['release'],
    dynamic_alerts: true,
  }
}

interface SeriesPoint {
  bucket_start: string
  sessions: number
  crash_free_sessions?: number | null
}

/** Chart rows: crash-free sessions per hour as a percentage. */
export function crashFreeChartRows(points: SeriesPoint[]) {
  return points.map((point) => ({
    time: point.bucket_start,
    hour: new Date(point.bucket_start).toLocaleString([], {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
    }),
    sessions: point.sessions,
    crashFree:
      point.crash_free_sessions == null
        ? null
        : Math.round(point.crash_free_sessions * 10000) / 100,
  }))
}
