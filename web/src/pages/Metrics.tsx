// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Routes, Route, Link, useLocation } from 'react-router'
import { RedirectWithQuery } from '@/components/routing/RedirectWithQuery'
import { ProjectResponse } from '@/api/client'
import { ChevronRight } from 'lucide-react'
import { useQuery } from '@tanstack/react-query'
import { listDashboardsOptions } from '@/api/client/@tanstack/react-query.gen'
import MetricsExplorer from './MetricsExplorer'
import DashboardsRouter from './DashboardsRouter'
import AlertsRouter from './AlertsRouter'
import { ruleStatus, useAlertStatus } from '@/components/metrics/alert-status'
import { alertSummary, StatusDot } from '@/components/metrics/alert-format'
import { firingRuleLinks } from '@/components/metrics/firing-rule-links'
import { Skeleton } from '@/components/ui/skeleton'

interface MetricsProps {
  project: ProjectResponse
}

/**
 * The Metrics pages of a project, each a tab of the Monitoring section:
 *   - Explore (`explore`): the all-metrics overview + per-metric drill-in.
 *   - Dashboards (`dashboards/*`): saved, curated dashboards.
 *   - Alerts (`alerts/*`): metric alert rules.
 *
 * The index is *dashboard-first*: a project with at least one saved dashboard
 * lands straight on it (no extra click), falling back to Explore only when
 * there are none.
 *
 * Firing alerts are listed above every page, and only when something is
 * actually wrong, so a healthy project keeps the charts high on the page.
 */
export default function Metrics({ project }: MetricsProps) {
  return (
    <div className="flex w-full flex-col gap-4">
      <MetricsFiringRows project={project} />
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

  if (wantsExplore) return <RedirectWithQuery to="explore" />

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
    return <RedirectWithQuery to={`dashboards/${dashboards[0].id}`} />
  }
  return <RedirectWithQuery to="explore" />
}

/** The `…/metrics` base path, derived so tab links resolve absolutely. */
function navBase(pathname: string): string {
  const i = pathname.indexOf('/metrics')
  return i === -1 ? pathname : pathname.slice(0, i + '/metrics'.length)
}

/** The Metrics base path and the project's alert status. */
function useMetricsNav(project: ProjectResponse) {
  const { pathname } = useLocation()
  // Cached by react-query — no extra fetch beyond what the pages already do.
  const status = useAlertStatus(project.id)
  return { base: navBase(pathname), status }
}

/** The firing-alert rows, above whichever Metrics page is open. */
function MetricsFiringRows({ project }: { project: ProjectResponse }) {
  const { base, status } = useMetricsNav(project)
  if (status.firing.length === 0) return null
  return (
    <FiringRows firing={status.firing} base={base} projectId={project.id} />
  )
}

/** The worst-first firing list, shown under the tab strip only when firing. */
function FiringRows({
  firing,
  base,
  projectId,
}: {
  firing: ReturnType<typeof useAlertStatus>['firing']
  base: string
  projectId: number
}) {
  return (
    <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-card">
      {firing.map((rule) => {
        const links = firingRuleLinks(rule, base, projectId)
        return (
          <li
            key={rule.id}
            className="flex flex-col gap-1.5 px-3 py-2 text-sm sm:flex-row sm:items-center sm:gap-2.5"
          >
            {/* The row itself opens the chart of the metric that fired. */}
            <Link
              to={links.metric}
              className="flex min-w-0 flex-1 items-center gap-2.5 rounded-sm hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <StatusDot level={ruleStatus(rule)} pulse />
              <span className="flex min-w-0 flex-1 flex-col sm:flex-row sm:items-baseline sm:gap-2">
                <span className="truncate font-medium">{rule.name}</span>
                <span className="truncate font-mono text-xs text-muted-foreground">
                  {alertSummary(rule)}
                </span>
              </span>
            </Link>
            <span className="flex shrink-0 items-center gap-3 pl-5 text-xs sm:pl-0">
              <Link
                to={links.alarm}
                className="text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
              >
                {links.alarmLabel}
              </Link>
              <Link
                to={links.rule}
                className="text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
              >
                Edit rule
              </Link>
              <ChevronRight className="hidden size-4 text-muted-foreground sm:block" />
            </span>
          </li>
        )
      })}
    </ul>
  )
}
