export type ProjectAnalyticsMetric =
  'visitors' | 'sessions' | 'returning_visitors' | 'page_views'

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
