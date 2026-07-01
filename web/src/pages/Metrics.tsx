import { Routes, Route, Link, Navigate, useLocation } from 'react-router-dom'
import { ProjectResponse } from '@/api/client'
import { Bell, ChevronRight, LayoutDashboard, LineChart } from 'lucide-react'
import { useQuery } from '@tanstack/react-query'
import { listDashboardsOptions } from '@/api/client/@tanstack/react-query.gen'
import MetricsExplorer from './MetricsExplorer'
import DashboardsRouter from './DashboardsRouter'
import AlertsRouter from './AlertsRouter'
import { ruleStatus, useAlertStatus } from '@/components/metrics/alert-status'
import { alertSummary, StatusDot } from '@/components/metrics/alert-format'
import { Skeleton } from '@/components/ui/skeleton'

interface MetricsProps {
  project: ProjectResponse
}

/**
 * Unified Metrics surface. One nav entry holds three modes:
 *   - Explore (`explore`): the all-metrics overview + per-metric drill-in.
 *   - Dashboards (`dashboards/*`): saved, curated dashboards.
 *   - Alerts (`alerts/*`): metric alert rules.
 *
 * The index is *dashboard-first*: a project with at least one saved dashboard
 * lands straight on it (no extra click), falling back to Explore only when
 * there are none. Explore lives at its own `explore` path so it stays reachable
 * from the tab even though it's no longer the index.
 *
 * A compact underline tab strip sits above the content with a small inline
 * health indicator — the firing-alert detail only expands when something is
 * actually wrong, so a healthy project keeps the charts high on the page.
 */
export default function Metrics({ project }: MetricsProps) {
  return (
    <div className="flex w-full flex-col gap-4">
      <MetricsTopBar project={project} />
      <Routes>
        <Route index element={<MetricsIndexRedirect project={project} />} />
        <Route path="explore" element={<MetricsExplorer project={project} />} />
        <Route
          path="dashboards/*"
          element={<DashboardsRouter project={project} />}
        />
        <Route path="alerts/*" element={<AlertsRouter project={project} />} />
      </Routes>
    </div>
  )
}

/**
 * Dashboard-first landing. Sends the index straight to the first saved
 * dashboard; with none (or on a load failure) it falls back to Explore so the
 * surface always renders something.
 */
function MetricsIndexRedirect({ project }: { project: ProjectResponse }) {
  const { search } = useLocation()
  // A bookmarked Explore deep link (`?metric=…`) keeps its intent: send it to
  // Explore with the query string intact rather than to a dashboard.
  const wantsExplore = new URLSearchParams(search).has('metric')

  const query = useQuery({
    ...listDashboardsOptions({ query: { project_id: project.id } }),
    enabled: !!project.id && !wantsExplore,
  })

  if (wantsExplore) return <Navigate to={`explore${search}`} replace />

  if (query.isPending) {
    return (
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {[0, 1, 2].map((i) => (
          <Skeleton key={i} className="h-[190px] w-full rounded-lg" />
        ))}
      </div>
    )
  }

  const dashboards = query.data?.data ?? []
  if (!query.isError && dashboards.length > 0) {
    return <Navigate to={`dashboards/${dashboards[0].id}`} replace />
  }
  return <Navigate to={`explore${search}`} replace />
}

interface TabDef {
  to: string
  label: string
  icon: typeof LineChart
  active: boolean
  firing: number
}

/** The `…/metrics` base path, derived so tab links resolve absolutely. */
function navBase(pathname: string): string {
  const i = pathname.indexOf('/metrics')
  return i === -1 ? pathname : pathname.slice(0, i + '/metrics'.length)
}

/** Shared nav model: base path, the three tabs, and the alert status model. */
function useMetricsNav(project: ProjectResponse) {
  const { pathname } = useLocation()
  const base = navBase(pathname)
  const onDashboards = pathname.startsWith(`${base}/dashboards`)
  const onAlerts = pathname.startsWith(`${base}/alerts`)

  // Cached by react-query — no extra fetch beyond what the surfaces already do.
  const status = useAlertStatus(project.id)

  const tabs: TabDef[] = [
    {
      // Explore has its own path now that the index is dashboard-first.
      to: `${base}/explore`,
      label: 'Explore',
      icon: LineChart,
      // Active for both the (transient) index and the explicit explore path.
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
      firing: status.firing.length,
    },
  ]

  return { base, tabs, status }
}

/** Minimal underline tab strip + inline health dot. */
function MetricsTopBar({ project }: { project: ProjectResponse }) {
  const { base, tabs, status } = useMetricsNav(project)
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-3 border-b border-border">
        <nav className="flex items-center gap-5">
          {tabs.map((t) => (
            <Link
              key={t.label}
              to={t.to}
              className={[
                'inline-flex items-center gap-1.5 border-b-2 px-0.5 pb-2 pt-1 text-sm font-medium transition-colors',
                t.active
                  ? 'border-primary text-foreground'
                  : 'border-transparent text-muted-foreground hover:text-foreground',
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
        </nav>
        <HealthDotInline project={project} base={base} />
      </div>
      {/* Firing rows surface only when something is actually wrong. */}
      {status.firing.length > 0 && (
        <FiringRows firing={status.firing} base={base} />
      )}
    </div>
  )
}

/**
 * Lean status indicator: a green "Healthy" dot when all clear, a red "N firing"
 * link when something is wrong, and an honest "Not watching" when no rule exists
 * — never a full-width band.
 */
function HealthDotInline({
  project,
  base,
}: {
  project: ProjectResponse
  base: string
}) {
  const status = useAlertStatus(project.id)

  if (status.isLoading) return <Skeleton className="h-4 w-16 rounded" />
  if (status.isError)
    return <span className="text-xs text-muted-foreground">—</span>
  if (!status.hasRules)
    return (
      <Link
        to={`${base}/alerts/new`}
        className="text-xs text-muted-foreground transition-colors hover:text-foreground"
      >
        Not watching
      </Link>
    )
  if (status.firing.length > 0)
    return (
      <Link
        to={`${base}/alerts`}
        className="inline-flex items-center gap-1.5 text-xs font-semibold text-destructive"
      >
        <span className="size-2 rounded-full bg-destructive" />
        {status.firing.length} firing
      </Link>
    )
  return (
    <span className="inline-flex items-center gap-1.5 text-xs font-medium text-emerald-600 dark:text-emerald-400">
      <span className="size-2 rounded-full bg-emerald-500" />
      Healthy
    </span>
  )
}

/** The worst-first firing list, shown under the tab strip only when firing. */
function FiringRows({
  firing,
  base,
}: {
  firing: ReturnType<typeof useAlertStatus>['firing']
  base: string
}) {
  return (
    <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-card">
      {firing.map((rule) => (
        <li key={rule.id}>
          <Link
            to={`${base}/alerts/${rule.id}/edit`}
            className="flex items-center gap-2.5 px-3 py-2 text-sm transition-colors hover:bg-muted/50"
          >
            <StatusDot level={ruleStatus(rule)} pulse />
            <span className="flex min-w-0 flex-1 flex-col sm:flex-row sm:items-baseline sm:gap-2">
              <span className="truncate font-medium">{rule.name}</span>
              <span className="truncate font-mono text-xs text-muted-foreground">
                {alertSummary(rule)}
              </span>
            </span>
            <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
          </Link>
        </li>
      ))}
    </ul>
  )
}
