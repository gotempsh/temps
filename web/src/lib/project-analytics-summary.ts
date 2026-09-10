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
