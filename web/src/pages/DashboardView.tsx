import { ProjectResponse } from '@/api/client'
// REGEN: bun run openapi-ts — getDashboardOptions is generated from the new
// GET /otel/dashboards/{id} endpoint (operationId get_dashboard). Not present
// in the committed SDK; imported so the page is regen-ready (project rule:
// always use the generated SDK, never hand-roll fetch).
import { getDashboardOptions } from '@/api/client/@tanstack/react-query.gen'
import { MetricTile } from '@/components/metrics/MetricTile'
import {
  rollupStatus,
  useAlertStatus,
} from '@/components/metrics/alert-status'
import {
  dashboardTiles,
  DashboardStatusBadge,
  FiringCount,
} from '@/components/metrics/dashboard-status'
import {
  RANGE_BUCKET,
  TIME_RANGES,
  type TimeRange,
  timeRangeToFrom,
} from '@/components/metrics/time-range'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { ArrowLeft, LayoutDashboard, Pencil } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'

interface DashboardViewProps {
  project: ProjectResponse
}

export default function DashboardView({ project }: DashboardViewProps) {
  const navigate = useNavigate()
  const { dashboardId } = useParams()
  const id = Number(dashboardId)
  const [timeRange, setTimeRange] = useState<TimeRange>('24h')

  // Memoize both time bounds so the per-tile query keys stay STABLE across
  // renders — an inline `new Date()` would change every render and spin React
  // Query into an infinite refetch loop (the bug fixed in MetricsExplorer).
  const fromIso = useMemo(
    () => timeRangeToFrom(timeRange, Date.now()).toISOString(),
    [timeRange],
  )
  const toIso = useMemo(
    () => new Date().toISOString(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [timeRange],
  )
  const bucketInterval = RANGE_BUCKET[timeRange]

  const dashboardQuery = useQuery({
    ...getDashboardOptions({ path: { id }, query: { project_id: project.id } }),
    enabled: Number.isFinite(id),
  })

  const dashboard = dashboardQuery.data
  usePageTitle(
    dashboard ? `${dashboard.name} · Dashboards` : 'Dashboard',
  )

  const sections = dashboard?.layout?.sections ?? []

  // Datadog-style firing status for this dashboard, derived from the same
  // cached alert-rule fetch the tiles use (no extra round-trip). Rolled up at
  // the dashboard level for the header summary and per section for the section
  // headers, so "what's on fire" reads at a glance without scanning every tile.
  const statusModel = useAlertStatus(project.id)
  const rollup = rollupStatus(dashboardTiles(sections), statusModel.rulesFor)

  return (
    <div className="flex w-full flex-col gap-4">
      {/* Header */}
      <div className="flex flex-col gap-2">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => navigate('..')}
          className="-mb-1 gap-1.5 self-start px-2 text-xs text-muted-foreground"
        >
          <ArrowLeft className="size-3.5" />
          All dashboards
        </Button>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex min-w-0 items-center gap-2">
            <LayoutDashboard className="size-5 shrink-0 text-muted-foreground" />
            <h1 className="truncate text-lg font-semibold tracking-tight">
              {dashboardQuery.isPending ? (
                <Skeleton className="h-6 w-40" />
              ) : (
                (dashboard?.name ?? 'Dashboard')
              )}
            </h1>
            {!dashboardQuery.isPending && <DashboardStatusBadge rollup={rollup} />}
          </div>
          <div className="flex items-center gap-2 self-start">
            <Select
              value={timeRange}
              onValueChange={(v) => setTimeRange(v as TimeRange)}
            >
              <SelectTrigger className="w-full sm:w-[160px]">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TIME_RANGES.map((r) => (
                  <SelectItem key={r.value} value={r.value}>
                    {r.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button
              variant="outline"
              size="sm"
              onClick={() => navigate('edit')}
              className="gap-1.5"
              disabled={!dashboard}
            >
              <Pencil className="size-3.5" />
              Edit
            </Button>
          </div>
        </div>
      </div>

      {dashboardQuery.isPending ? (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3">
          {[0, 1, 2, 3, 4, 5].map((i) => (
            <Skeleton key={i} className="h-[220px] w-full rounded-lg" />
          ))}
        </div>
      ) : dashboardQuery.isError ? (
        <div className="rounded-lg border border-border bg-card p-4">
          <EmptyState
            icon={LayoutDashboard}
            title="Dashboard not found"
            description="This dashboard could not be loaded. It may have been deleted."
            action={
              <Button size="sm" variant="outline" onClick={() => navigate('..')}>
                Back to dashboards
              </Button>
            }
          />
        </div>
      ) : sections.length === 0 ? (
        <div className="rounded-lg border border-border bg-card p-4">
          <EmptyState
            icon={LayoutDashboard}
            title="This dashboard is empty"
            description="Edit the dashboard to add sections and metric tiles."
            action={
              <Button
                size="sm"
                onClick={() => navigate('edit')}
                className="gap-1.5"
              >
                <Pencil className="size-4" />
                Edit dashboard
              </Button>
            }
          />
        </div>
      ) : (
        <div className="flex flex-col gap-6">
          {sections.map((section) => {
            const sectionRollup = rollupStatus(
              dashboardTiles([section]),
              statusModel.rulesFor,
            )
            return (
            <section key={section.id} className="flex flex-col gap-3">
              <h2 className="flex items-center gap-2 text-sm font-semibold tracking-tight">
                {section.title || 'Untitled section'}
                <FiringCount rollup={sectionRollup} className="text-xs" />
              </h2>
              {(section.tiles?.length ?? 0) === 0 ? (
                <p className="text-xs text-muted-foreground">
                  No tiles in this section.
                </p>
              ) : (
                <div className="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3">
                  {section.tiles.map((tile) => (
                    <MetricTile
                      key={tile.id}
                      project={project}
                      metricName={tile.metric_name}
                      aggregation={tile.aggregation}
                      title={tile.title}
                      fromIso={fromIso}
                      toIso={toIso}
                      bucketInterval={bucketInterval}
                    />
                  ))}
                </div>
              )}
            </section>
            )
          })}
        </div>
      )}
    </div>
  )
}
