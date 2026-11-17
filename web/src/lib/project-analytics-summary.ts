// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Raw session IDs include browser-like automation and are not human KPIs. */
export const PROJECT_OVERVIEW_ANALYTICS_METRICS = [
  'visitors',
  'page_views',
  'returning_visitors',
] as const

export type ProjectAnalyticsMetric =
  (typeof PROJECT_OVERVIEW_ANALYTICS_METRICS)[number]

/**
 * Whether the setup wizard's check has proved anything. A project that never
 * had an event passes on its first. One that already had events always "has
 * events", so only an event since the check started shows the snippet works.
 */
export function analyticsCheckPassed(check: {
  hadEventsBefore: boolean
  hasEvents: boolean | undefined
  eventsSinceCheck: number | undefined
}): boolean {
  return check.hadEventsBefore
    ? (check.eventsSinceCheck ?? 0) > 0
    : check.hasEvents === true
}

/**
 * The window a check looks at: from a minute before it started, so a clock a
 * little ahead of the server's still sees the event, for as long as it runs.
 */
export function analyticsCheckWindow(startedAt: number): AnalyticsWindow {
  return {
    start_date: new Date(startedAt - 60_000).toISOString(),
    end_date: new Date(startedAt + 24 * 60 * 60_000).toISOString(),
  }
}

interface AnalyticsWindow {
  start_date: string
  end_date: string
}

/**
 * Builds the project-scoped request used by project overview headline metrics.
 * Keep these metrics on `/projects/{project_id}/unique-counts`; the legacy
 * general-stats endpoint is instance-wide and cannot safely power project UI.
 */
export function buildProjectAnalyticsCountRequest(
  projectId: number,
  window: AnalyticsWindow,
  metric: ProjectAnalyticsMetric
) {
  return {
    path: { project_id: projectId },
    query: { ...window, metric },
  }
}
