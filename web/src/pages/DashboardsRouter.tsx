import { Routes, Route } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import Dashboards from './Dashboards'
import DashboardView from './DashboardView'
import DashboardBuilder from './DashboardBuilder'

interface DashboardsRouterProps {
  project: ProjectResponse
}

/**
 * Metric-dashboards section router. Mirrors `Metrics.tsx`: an index list, a
 * create/edit builder, and a per-dashboard view. Mounted at `dashboards/*`
 * under the project (see ProjectDetail routes).
 */
export default function DashboardsRouter({ project }: DashboardsRouterProps) {
  return (
    <Routes>
      <Route index element={<Dashboards project={project} />} />
      <Route path="new" element={<DashboardBuilder project={project} />} />
      <Route path=":dashboardId" element={<DashboardView project={project} />} />
      <Route
        path=":dashboardId/edit"
        element={<DashboardBuilder project={project} />}
      />
    </Routes>
  )
}
