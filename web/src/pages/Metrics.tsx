import { Routes, Route, Link, useLocation } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import { Bell, LayoutDashboard, LineChart } from 'lucide-react'
import MetricsExplorer from './MetricsExplorer'
import DashboardsRouter from './DashboardsRouter'
import AlertsRouter from './AlertsRouter'
import { MetricsHealthHeader } from '@/components/metrics/MetricsHealthHeader'
import { useAlertStatus } from '@/components/metrics/alert-status'

interface MetricsProps {
  project: ProjectResponse
}

/**
 * Unified Metrics surface. One nav entry holds two modes:
 *   - Explore (index): the all-metrics overview + per-metric drill-in.
 *   - Dashboards (`dashboards/*`): saved, curated dashboards.
 *
 * Explore stays at the `metrics` index (preserving existing links), and the
 * dashboards section is nested *unchanged* under `metrics/dashboards/*` so the
 * dashboard pages' relative navigation keeps working.
 */
export default function Metrics({ project }: MetricsProps) {
  return (
    <div className="flex w-full flex-col gap-4">
      <MetricsHealthHeader project={project} />
      <MetricsTabs project={project} />
      <Routes>
        <Route index element={<MetricsExplorer project={project} />} />
        <Route
          path="dashboards/*"
          element={<DashboardsRouter project={project} />}
        />
        <Route path="alerts/*" element={<AlertsRouter project={project} />} />
      </Routes>
    </div>
  )
}

/** Route-backed segmented control switching between Explore and Dashboards. */
function MetricsTabs({ project }: { project: ProjectResponse }) {
  const { pathname } = useLocation()
  // The hub is mounted at `…/metrics/*`; derive that base so the tab links are
  // absolute (relative links would resolve against the current sub-route).
  const i = pathname.indexOf('/metrics')
  const base = i === -1 ? pathname : pathname.slice(0, i + '/metrics'.length)
  const onDashboards = pathname.startsWith(`${base}/dashboards`)
  const onAlerts = pathname.startsWith(`${base}/alerts`)

  // Cached by react-query (same key as the header) — no extra fetch.
  const { firing } = useAlertStatus(project.id)

  const tabs = [
    {
      to: base,
      label: 'Explore',
      icon: LineChart,
      active: !onDashboards && !onAlerts,
      firing: 0,
    },
    {
      to: `${base}/dashboards`,
      label: 'Dashboards',
      icon: LayoutDashboard,
      active: onDashboards,
      firing: 0,
    },
    {
      to: `${base}/alerts`,
      label: 'Alerts',
      icon: Bell,
      active: onAlerts,
      firing: firing.length,
    },
  ]

  return (
    <div className="inline-flex items-center gap-1 self-start rounded-lg border border-border bg-muted/40 p-1">
      {tabs.map((t) => (
        <Link
          key={t.label}
          to={t.to}
          className={[
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors',
            t.active
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground',
          ].join(' ')}
        >
          <t.icon className="size-4" />
          {t.label}
          {t.firing > 0 && (
            <span className="inline-flex min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[10px] font-semibold tabular-nums text-destructive-foreground">
              {t.firing}
            </span>
          )}
        </Link>
      ))}
    </div>
  )
}
