import { ProjectResponse } from '@/api/client'
// REGEN: bun run openapi-ts — these option/mutation builders are generated from
// the new /otel/alerts endpoints (operationIds list_alerts / delete_alert).
// They do not exist in the committed SDK yet; import them so the page is
// regen-ready rather than hand-rolling fetch (project rule: always use the
// generated SDK).
import {
  listAlertsOptions,
  deleteAlertMutation,
} from '@/api/client/@tanstack/react-query.gen'
// REGEN: OtelMetricAlertRuleResponse comes from the regenerated types.gen.
import type { OtelMetricAlertRuleResponse } from '@/api/client'
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
  Bell,
  ChevronRight,
  MoreVertical,
  Pencil,
  Plus,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import {
  AlertStateBadge,
  alertSummary,
  StatusDot,
} from '@/components/metrics/alert-format'
import { ruleStatus, statusRank } from '@/components/metrics/alert-status'

interface MetricAlertsProps {
  project: ProjectResponse
}

export default function MetricAlerts({ project }: MetricAlertsProps) {
  usePageTitle(`Alerts · ${project.name}`)
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [pendingDelete, setPendingDelete] =
    useState<OtelMetricAlertRuleResponse | null>(null)

  const alertsQuery = useQuery({
    ...listAlertsOptions({ query: { project_id: project.id } }),
    enabled: !!project.id,
  })

  const deleteMutation = useMutation({
    ...deleteAlertMutation(),
    meta: { errorTitle: 'Failed to delete alert rule' },
    onSuccess: () => {
      toast.success('Alert rule deleted')
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id === 'listAlerts',
      })
      setPendingDelete(null)
    },
  })

  // Worst-first: firing-critical → firing-warning → unknown → ok, then by name.
  const alerts = [...(alertsQuery.data?.data ?? [])].sort(
    (a, b) =>
      statusRank(ruleStatus(a)) - statusRank(ruleStatus(b)) ||
      a.name.localeCompare(b.name),
  )

  const goToNew = () => navigate('new')

  return (
    <div className="flex w-full flex-col gap-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <Bell className="size-5 text-muted-foreground" />
            <h1 className="text-lg font-semibold tracking-tight">Alerts</h1>
          </div>
          <p className="text-sm text-muted-foreground">
            Threshold alerts on OpenTelemetry metrics for {project.name}.
          </p>
        </div>
        <Button size="sm" onClick={goToNew} className="gap-1.5 self-start">
          <Plus className="size-4" />
          New alert
        </Button>
      </div>

      {alertsQuery.isPending ? (
        <div className="flex flex-col gap-1">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-14 w-full rounded-lg" />
          ))}
        </div>
      ) : alertsQuery.isError ? (
        <div className="rounded-lg border border-border bg-card p-4">
          <EmptyState
            icon={Bell}
            title="Failed to load alerts"
            description="Something went wrong fetching your alert rules. Try refreshing the page."
          />
        </div>
      ) : alerts.length === 0 ? (
        <div className="rounded-lg border border-border bg-card p-4">
          <EmptyState
            icon={Bell}
            title="No alert rules yet"
            description="Create an alert to get notified when a metric crosses a threshold. Alerts evaluate on a schedule and fire through your configured notification channels."
            action={
              <Button size="sm" onClick={goToNew} className="gap-1.5">
                <Plus className="size-4" />
                New alert
              </Button>
            }
          />
        </div>
      ) : (
        <div className="flex flex-col">
          {alerts.map((rule) => (
            <AlertRow
              key={rule.id}
              rule={rule}
              onEdit={() => navigate(`${rule.id}/edit`)}
              onDelete={() => setPendingDelete(rule)}
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
            <AlertDialogTitle>Delete alert rule?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently deletes
              {pendingDelete ? ` “${pendingDelete.name}”` : ' this alert rule'}.
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

function AlertRow({
  rule,
  onEdit,
  onDelete,
}: {
  rule: OtelMetricAlertRuleResponse
  onEdit: () => void
  onDelete: () => void
}) {
  return (
    <div className="group flex items-center gap-3 rounded-lg px-2 py-2.5 transition hover:bg-muted/50">
      <StatusDot
        level={ruleStatus(rule)}
        pulse
        className="ml-1"
        title={`Status: ${rule.last_state}`}
      />
      <button
        type="button"
        onClick={onEdit}
        className="flex min-w-0 flex-1 items-center gap-3 text-left"
      >
        <div className="flex size-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <Bell className="size-4" />
        </div>
        <div className="flex min-w-0 flex-col">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-medium">{rule.name}</span>
            {!rule.enabled && (
              <span className="shrink-0 text-[10px] uppercase tracking-wide text-muted-foreground">
                disabled
              </span>
            )}
          </div>
          <span className="truncate font-mono text-xs text-muted-foreground">
            {alertSummary(rule)}
          </span>
        </div>
      </button>

      <AlertStateBadge state={rule.last_state} />

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="size-8 shrink-0"
            aria-label="Alert actions"
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
