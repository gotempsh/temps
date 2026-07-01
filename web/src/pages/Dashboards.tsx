import { ProjectResponse } from '@/api/client'
// REGEN: bun run openapi-ts — these option/mutation builders are generated from
// the new /otel/dashboards endpoints (operationIds list_dashboards /
// delete_dashboard). They do not exist in the committed SDK yet; import them so
// the page is regen-ready rather than hand-rolling fetch (project rule: always
// use the generated SDK).
import {
  listDashboardsOptions,
  deleteDashboardMutation,
} from '@/api/client/@tanstack/react-query.gen'
// REGEN: OtelDashboardResponse comes from the regenerated types.gen.
import type { OtelDashboardResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ChevronRight,
  LayoutDashboard,
  MoreVertical,
  Pencil,
  Plus,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { StatusDot } from '@/components/metrics/alert-format'
import {
  rollupStatus,
  useAlertStatus,
  type StatusRollup,
} from '@/components/metrics/alert-status'
import {
  dashboardTiles,
  FiringCount,
} from '@/components/metrics/dashboard-status'

interface DashboardsProps {
  project: ProjectResponse
}

/** Count sections + tiles for the compact one-line row description. */
function dashboardSummary(d: OtelDashboardResponse): string {
  const sections = d.layout?.sections ?? []
  const sectionCount = sections.length
  const tileCount = sections.reduce((n, s) => n + (s.tiles?.length ?? 0), 0)
  const sectionLabel = `${sectionCount} section${sectionCount === 1 ? '' : 's'}`
  const tileLabel = `${tileCount} tile${tileCount === 1 ? '' : 's'}`
  return `${sectionLabel} · ${tileLabel}`
}

export default function Dashboards({ project }: DashboardsProps) {
  usePageTitle(`Dashboards · ${project.name}`)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [pendingDelete, setPendingDelete] =
    useState<OtelDashboardResponse | null>(null)

  const dashboardsQuery = useQuery({
    ...listDashboardsOptions({ query: { project_id: project.id } }),
    enabled: !!project.id,
  })

  const deleteMutation = useMutation({
    ...deleteDashboardMutation(),
    meta: { errorTitle: 'Failed to delete dashboard' },
    onSuccess: () => {
      toast.success('Dashboard deleted')
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id ===
          'listDashboards',
      })
      setPendingDelete(null)
    },
  })

  const dashboards = dashboardsQuery.data?.data ?? []

  // Datadog-style: derive each dashboard's status from the metrics its tiles
  // plot vs. the project's firing alert rules. One cached listAlerts fetch — the
  // same the per-tile dots use — so the row badge can't disagree with the tiles.
  const statusModel = useAlertStatus(project.id)

  const goToNew = () => navigate('new')

  return (
    <div className="flex w-full flex-col gap-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <LayoutDashboard className="size-5 text-muted-foreground" />
            <h1 className="text-lg font-semibold tracking-tight">Dashboards</h1>
          </div>
          <p className="text-sm text-muted-foreground">
            Saved metric dashboards for {project.name}.
          </p>
        </div>
        <Button size="sm" onClick={goToNew} className="gap-1.5 self-start">
          <Plus className="size-4" />
          New dashboard
        </Button>
      </div>

      {dashboardsQuery.isPending ? (
        <div className="flex flex-col gap-1">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-14 w-full rounded-lg" />
          ))}
        </div>
      ) : dashboardsQuery.isError ? (
        <div className="rounded-lg border border-border bg-card p-4">
          <EmptyState
            icon={LayoutDashboard}
            title="Failed to load dashboards"
            description="Something went wrong fetching your dashboards. Try refreshing the page."
          />
        </div>
      ) : dashboards.length === 0 ? (
        <div className="rounded-lg border border-border bg-card p-4">
          <EmptyState
            icon={LayoutDashboard}
            title="No dashboards yet"
            description="Create a dashboard to pin the OpenTelemetry metrics you care about into sections of charts."
            action={
              <Button size="sm" onClick={goToNew} className="gap-1.5">
                <Plus className="size-4" />
                New dashboard
              </Button>
            }
          />
        </div>
      ) : (
        <div className="flex flex-col">
          {dashboards.map((d) => (
            <DashboardRow
              key={d.id}
              dashboard={d}
              rollup={rollupStatus(
                dashboardTiles(d.layout?.sections),
                statusModel.rulesFor,
              )}
              onOpen={() => navigate(String(d.id))}
              onEdit={() => navigate(`${d.id}/edit`)}
              onDelete={() => setPendingDelete(d)}
            />
          ))}
        </div>
      )}

      <AlertDialog
        open={!!pendingDelete}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete dashboard?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently deletes
              {pendingDelete ? ` “${pendingDelete.name}”` : ' this dashboard'}.
              This action cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={deleteMutation.isPending}
              onClick={(e) => {
                e.preventDefault()
                if (pendingDelete) {
                  deleteMutation.mutate({
                    path: { id: pendingDelete.id },
                    query: { project_id: project.id },
                  })
                }
              }}
            >
              {deleteMutation.isPending ? 'Deleting…' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function DashboardRow({
  dashboard,
  rollup,
  onOpen,
  onEdit,
  onDelete,
}: {
  dashboard: OtelDashboardResponse
  rollup: StatusRollup
  onOpen: () => void
  onEdit: () => void
  onDelete: () => void
}) {
  const firing = rollup.firing > 0 && rollup.level !== null
  return (
    <div className="group flex items-center gap-3 rounded-lg px-2 py-2.5 transition hover:bg-muted/50">
      <button
        type="button"
        onClick={onOpen}
        className="flex min-w-0 flex-1 items-center gap-3 text-left"
      >
        <div className="relative flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <LayoutDashboard className="size-4" />
          {firing && rollup.level && (
            <span className="absolute -right-0.5 -top-0.5">
              <StatusDot
                level={rollup.level}
                pulse
                title={`${rollup.firing} firing`}
              />
            </span>
          )}
        </div>
        <div className="flex min-w-0 flex-col">
          <span className="truncate text-sm font-medium">{dashboard.name}</span>
          <span className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
            {dashboardSummary(dashboard)}
            {firing && (
              <>
                <span aria-hidden>·</span>
                <FiringCount rollup={rollup} />
              </>
            )}
          </span>
        </div>
      </button>

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="size-8 shrink-0"
            aria-label="Dashboard actions"
          >
            <MoreVertical className="size-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={onEdit}>
            <Pencil className="size-4" />
            Edit
          </DropdownMenuItem>
          <DropdownMenuItem
            onClick={onDelete}
            className="text-rose-500 focus:text-rose-500"
          >
            <Trash2 className="size-4" />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
    </div>
  )
}
