// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * NodeAlertRules — monitoring alert rules of one node.
 *
 * For the control plane (synthetic node 0, the default) these are the seeded
 * proxy health alerts (error rate, p99 latency) and the file-descriptor /
 * socket exhaustion alerts. Nothing else in the console surfaces them, so
 * without this panel the operator has no way to learn that the platform is
 * watching itself, let alone retune or silence a rule.
 *
 * For a worker (`nodeId` > 0) the operator adds rules on CPU, memory and disk
 * usage, one per metric, and can delete them.
 *
 * The control plane's rules are update-only by design. The defaults are
 * re-seeded on every server start (`ON CONFLICT DO NOTHING`), so a deleted
 * rule silently reappears on the next restart — disabling is the operation
 * that actually sticks, which is why those rows offer a toggle and not a
 * delete.
 */
import {
  nodeMetricsCreateAlertRuleMutation,
  nodeMetricsDeleteAlertRuleMutation,
  nodeMetricsGetAlertRulesOptions,
  nodeMetricsUpdateAlertRuleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  NodeCreateAlertRuleRequest,
  ServiceAlertRuleResponse,
} from '@/api/client/types.gen'
import { AddNodeAlertRuleDialog } from '@/components/monitoring/AddNodeAlertRuleDialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Sparkles, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { ReadFailure } from '@/components/ui/read-failure'
import {
  NODE_ALERT_METRIC_LABELS,
  canDeleteNodeRule,
  canDeleteNodeRules,
  metricsWithoutRule,
  missingRecommendedAlerts,
  nodeAlertMetricLabel,
} from '@/lib/node-alert-rules'
import { isForbiddenRead } from '@/lib/read-failure'
import { toast } from 'sonner'

/** The control-plane is always node 0 — mirrors `CONTROL_PLANE_NODE_ID`. */
const CONTROL_PLANE_NODE_ID = 0

/**
 * Human labels for the metrics these rules watch. A raw `node.fd_percent` in
 * the table means nothing to an operator who has never read the collector.
 */
const METRIC_LABELS: Record<string, string> = {
  ...NODE_ALERT_METRIC_LABELS,
  'node.fd_percent': 'System file descriptors in use',
  'node.process_fd_percent': 'Process file descriptors in use',
  'proxy.error_rate_percent': 'Proxy 5xx rate',
  'proxy.request_duration_p99_ms': 'Proxy p99 latency',
}

/** Unit suffix per metric, so "90" reads as "90%" rather than a bare number. */
const METRIC_UNITS: Record<string, string> = {
  'node.cpu_percent': '%',
  'node.memory_percent': '%',
  'node.disk_percent': '%',
  'node.fd_percent': '%',
  'node.process_fd_percent': '%',
  'proxy.error_rate_percent': '%',
  'proxy.request_duration_p99_ms': 'ms',
}

function metricLabel(metric: string): string {
  return nodeAlertMetricLabel(metric) ?? METRIC_LABELS[metric] ?? metric
}

function metricUnit(metric: string): string {
  return METRIC_UNITS[metric] ?? ''
}

function formatDuration(secs: number): string {
  if (secs >= 3600)
    return `${(secs / 3600).toFixed(secs % 3600 === 0 ? 0 : 1)}h`
  if (secs >= 60) return `${(secs / 60).toFixed(secs % 60 === 0 ? 0 : 1)}m`
  return `${secs}s`
}

function severityVariant(
  severity: string
): 'default' | 'secondary' | 'destructive' | 'outline' {
  switch (severity) {
    case 'critical':
      return 'destructive'
    case 'warning':
      return 'default'
    default:
      return 'secondary'
  }
}

type RuleRowProps = {
  rule: ServiceAlertRuleResponse
  onThresholdSave: (rule: ServiceAlertRuleResponse, threshold: number) => void
  onToggle: (rule: ServiceAlertRuleResponse, enabled: boolean) => void
  /** Present only where deleting sticks (workers); the control plane re-seeds. */
  onDelete?: (rule: ServiceAlertRuleResponse) => void
  /** A default rule in a table that has a delete column: disable, not delete. */
  deleteProtected?: boolean
  isSaving: boolean
}

function RuleRow({
  rule,
  onThresholdSave,
  onToggle,
  onDelete,
  deleteProtected = false,
  isSaving,
}: RuleRowProps) {
  // `null` means "not being edited" — the cell shows the server value. Any
  // string means the operator is typing, so the input owns the value until
  // they save or cancel.
  const [draft, setDraft] = useState<string | null>(null)

  const parsed = draft === null ? null : Number(draft)
  const draftValid =
    parsed !== null && Number.isFinite(parsed) && draft?.trim() !== ''
  const isDirty = draftValid && parsed !== rule.threshold

  const commit = () => {
    if (!isDirty || parsed === null) return
    onThresholdSave(rule, parsed)
    setDraft(null)
  }

  return (
    <TableRow>
      <TableCell className="align-top">
        <div className="font-medium">{rule.name}</div>
        <div className="text-xs text-muted-foreground">
          {metricLabel(rule.metric_name)}
        </div>
      </TableCell>
      <TableCell className="align-top">
        <div className="flex items-center gap-1.5">
          <span className="text-sm text-muted-foreground">
            {rule.comparator}
          </span>
          <Input
            className="h-8 w-24 tabular-nums"
            inputMode="decimal"
            value={draft ?? String(rule.threshold)}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') commit()
              if (e.key === 'Escape') setDraft(null)
            }}
            aria-label={`Threshold for ${rule.name}`}
          />
          <span className="text-sm text-muted-foreground">
            {metricUnit(rule.metric_name)}
          </span>
        </div>
        {draft !== null && !draftValid && (
          <p className="mt-1 text-xs text-destructive">Enter a number</p>
        )}
        {isDirty && (
          <div className="mt-1 flex items-center gap-2">
            <Button
              size="sm"
              className="h-7"
              onClick={commit}
              disabled={isSaving}
            >
              Save
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-7"
              onClick={() => setDraft(null)}
            >
              Cancel
            </Button>
          </div>
        )}
      </TableCell>
      <TableCell className="hidden align-top tabular-nums md:table-cell">
        {formatDuration(rule.for_duration_secs)}
      </TableCell>
      <TableCell className="align-top">
        <Badge variant={severityVariant(rule.severity)}>{rule.severity}</Badge>
      </TableCell>
      <TableCell className="align-top">
        <Switch
          checked={rule.enabled}
          onCheckedChange={(checked) => onToggle(rule, checked)}
          disabled={isSaving}
          aria-label={`${rule.enabled ? 'Disable' : 'Enable'} ${rule.name}`}
        />
      </TableCell>
      {onDelete && deleteProtected && (
        <TableCell className="align-top">
          <span className="sr-only">
            Default rule: turn it off instead of deleting it
          </span>
        </TableCell>
      )}
      {onDelete && !deleteProtected && (
        <TableCell className="align-top">
          <Button
            size="icon"
            variant="ghost"
            className="size-8"
            onClick={() => onDelete(rule)}
            disabled={isSaving}
            aria-label={`Delete ${rule.name}`}
          >
            <Trash2 className="size-4" />
          </Button>
        </TableCell>
      )}
    </TableRow>
  )
}

export type NodeAlertRulesProps = {
  /** Node whose rules are shown; the control plane (`0`) when omitted. */
  nodeId?: number
}

function invalidateRules(queryClient: ReturnType<typeof useQueryClient>) {
  return queryClient.invalidateQueries({
    predicate: (query) =>
      (query.queryKey[0] as Record<string, unknown>)?._id ===
      'nodeMetricsGetAlertRules',
  })
}

export function NodeAlertRules({
  nodeId = CONTROL_PLANE_NODE_ID,
}: NodeAlertRulesProps = {}) {
  const queryClient = useQueryClient()
  const isControlPlane = nodeId === CONTROL_PLANE_NODE_ID
  const deletable = canDeleteNodeRules(nodeId)
  const subject = isControlPlane ? 'the control plane' : 'this worker'

  const [addOpen, setAddOpen] = useState(false)
  const [deleteTarget, setDeleteTarget] =
    useState<ServiceAlertRuleResponse | null>(null)
  const [recommendedPending, setRecommendedPending] = useState(false)

  const rulesQuery = useQuery({
    ...nodeMetricsGetAlertRulesOptions({
      path: { id: nodeId },
    }),
    staleTime: 30_000,
  })

  const updateMutation = useMutation({
    ...nodeMetricsUpdateAlertRuleMutation(),
    meta: { errorTitle: 'Failed to update the alert rule' },
    onSuccess: () => invalidateRules(queryClient),
  })

  const createMutation = useMutation({
    ...nodeMetricsCreateAlertRuleMutation(),
    meta: { errorTitle: 'Failed to add the alert rule' },
    onSuccess: () => invalidateRules(queryClient),
  })

  const deleteMutation = useMutation({
    ...nodeMetricsDeleteAlertRuleMutation(),
    meta: { errorTitle: 'Failed to delete the alert rule' },
    onSuccess: () => invalidateRules(queryClient),
  })

  const handleThresholdSave = (
    rule: ServiceAlertRuleResponse,
    threshold: number
  ) => {
    updateMutation.mutate(
      {
        path: { id: nodeId, rule_id: rule.id },
        body: { threshold },
      },
      {
        onSuccess: () =>
          toast.success(`${rule.name}: threshold set to ${threshold}`),
      }
    )
  }

  const handleToggle = (rule: ServiceAlertRuleResponse, enabled: boolean) => {
    updateMutation.mutate(
      {
        path: { id: nodeId, rule_id: rule.id },
        body: { enabled },
      },
      {
        onSuccess: () =>
          toast.success(`${rule.name} ${enabled ? 'enabled' : 'disabled'}`),
      }
    )
  }

  const rules = rulesQuery.data ?? []
  const availableMetrics = metricsWithoutRule(rules)
  const recommended = missingRecommendedAlerts(rules)

  const handleCreate = (body: NodeCreateAlertRuleRequest) => {
    createMutation.mutate(
      { path: { id: nodeId }, body },
      {
        onSuccess: () => {
          toast.success(`Alert rule "${body.name}" added`)
          setAddOpen(false)
        },
      }
    )
  }

  // Sequential on purpose: three small writes, and a failure on one (say the
  // rule appeared meanwhile) must not hide the outcome of the others.
  const handleAddRecommended = async () => {
    setRecommendedPending(true)
    let added = 0
    for (const rec of recommended) {
      try {
        await createMutation.mutateAsync({
          path: { id: nodeId },
          body: { ...rec, enabled: true },
        })
        added += 1
      } catch {
        // The mutation's error toast already names the failure.
      }
    }
    setRecommendedPending(false)
    if (added > 0) {
      toast.success(
        `Added ${added} recommended alert rule${added === 1 ? '' : 's'}`
      )
    }
  }

  const handleDeleteConfirm = () => {
    if (!deleteTarget) return
    const target = deleteTarget
    deleteMutation.mutate(
      { path: { id: nodeId, rule_id: target.id } },
      {
        onSuccess: () => {
          toast.success(`Alert rule "${target.name}" deleted`)
          setDeleteTarget(null)
        },
      }
    )
  }

  const recommendedLabel = recommended
    .map((r) => `${metricLabel(r.metric_name)} > ${r.threshold}%`)
    .join(', ')

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div className="space-y-1.5">
            <CardTitle className="text-base">
              {isControlPlane ? 'Platform alert rules' : 'Alert rules'}
            </CardTitle>
            <CardDescription>
              {isControlPlane
                ? 'Alerts the control-plane raises about itself — proxy health and file descriptor (socket) exhaustion. Seeded on startup; retune a threshold or switch a rule off here. You can also add rules on CPU, memory and disk usage. Deleting is deliberately not offered: the defaults are re-seeded on every restart, so only disabling sticks.'
                : 'Alerts raised when this worker runs short of CPU, memory or disk. Each metric has one rule; a rule fires once usage stays beyond its threshold for the configured time.'}
            </CardDescription>
          </div>
          <div className="flex flex-wrap gap-2">
            {recommended.length > 0 && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => void handleAddRecommended()}
                disabled={recommendedPending || !rulesQuery.isSuccess}
                title={recommendedLabel}
              >
                <Sparkles className="mr-2 size-4" />
                Add recommended alerts
              </Button>
            )}
            <Button
              size="sm"
              onClick={() => setAddOpen(true)}
              disabled={!rulesQuery.isSuccess}
            >
              <Plus className="mr-2 size-4" />
              <span className="hidden sm:inline">Add alert rule</span>
              <span className="sm:hidden">Add</span>
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {rulesQuery.isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : rulesQuery.isError ? (
          <ReadFailure
            resource={isControlPlane ? 'Platform alert rules' : 'Alert rules'}
            error={rulesQuery.error}
            onRetry={
              isForbiddenRead(rulesQuery.error)
                ? undefined
                : () => void rulesQuery.refetch()
            }
          />
        ) : rules.length === 0 ? (
          <p className="px-2 py-6 text-center text-sm text-muted-foreground">
            {isControlPlane
              ? 'No alert rules on the control-plane node yet. Defaults are seeded the next time the server starts, or add one now.'
              : `No alert rules for this worker yet. Add one, or use "Add recommended alerts" to be told when it passes ${recommendedLabel}.`}
          </p>
        ) : (
          <div className="overflow-x-auto">
            <Table className="min-w-[560px]">
              <TableHeader>
                <TableRow>
                  <TableHead>Rule</TableHead>
                  <TableHead>Fires when</TableHead>
                  <TableHead className="hidden md:table-cell">For</TableHead>
                  <TableHead>Severity</TableHead>
                  <TableHead>Enabled</TableHead>
                  {deletable && (
                    <TableHead className="w-12">
                      <span className="sr-only">Delete</span>
                    </TableHead>
                  )}
                </TableRow>
              </TableHeader>
              <TableBody>
                {rules.map((rule) => (
                  <RuleRow
                    key={rule.id}
                    rule={rule}
                    onThresholdSave={handleThresholdSave}
                    onToggle={handleToggle}
                    onDelete={deletable ? setDeleteTarget : undefined}
                    deleteProtected={
                      !canDeleteNodeRule(nodeId, rule.metric_name)
                    }
                    isSaving={updateMutation.isPending}
                  />
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>

      <AddNodeAlertRuleDialog
        open={addOpen}
        onOpenChange={setAddOpen}
        availableMetrics={availableMetrics}
        subject={subject}
        isPending={createMutation.isPending}
        onSubmit={handleCreate}
      />
      <ConfirmDeleteDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null)
        }}
        resourceType="alert rule"
        resourceName={deleteTarget?.name ?? ''}
        consequences="This worker stops being checked against this metric until you add a rule again."
        isPending={deleteMutation.isPending}
        onConfirm={handleDeleteConfirm}
      />
    </Card>
  )
}
