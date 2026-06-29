import { Routes, Route } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import MetricAlerts from './MetricAlerts'
import MetricAlertForm from './MetricAlertForm'

interface AlertsRouterProps {
  project: ProjectResponse
}

/**
 * Metric-alerts section router. Mirrors `DashboardsRouter`: an index list and a
 * create/edit form. Mounted at `alerts/*` under the unified Metrics surface
 * (see `Metrics.tsx`). An alert rule is defined on a signal (metric +
 * aggregation + comparator + threshold), independent of any dashboard.
 */
export default function AlertsRouter({ project }: AlertsRouterProps) {
  return (
    <Routes>
      <Route index element={<MetricAlerts project={project} />} />
      <Route path="new" element={<MetricAlertForm project={project} />} />
      <Route path=":alertId/edit" element={<MetricAlertForm project={project} />} />
    </Routes>
  )
}
