/**
 * NodeAlertRules — the control-plane's own monitoring alert rules.
 *
 * These rules live on the synthetic node 0 rather than on any project or
 * service: the seeded proxy health alerts (error rate, p99 latency) and the
 * file-descriptor / socket exhaustion alerts. Nothing else in the console
 * surfaces them, so without this panel the operator has no way to learn that
 * the platform is watching itself, let alone retune or silence a rule.
 *
 * Update-only by design. The defaults are re-seeded on every server start
 * (`ON CONFLICT DO NOTHING`), so a deleted rule silently reappears on the next
 * restart — disabling is the operation that actually sticks, which is why the
 * row offers a toggle and not a delete.
 */
import {
  nodeMetricsGetAlertRulesOptions,
  nodeMetricsUpdateAlertRuleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ServiceAlertRuleResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { useState } from 'react'
import { toast } from 'sonner'

/** The control-plane is always node 0 — mirrors `CONTROL_PLANE_NODE_ID`. */
const CONTROL_PLANE_NODE_ID = 0

/**
 * Human labels for the metrics these rules watch. A raw `node.fd_percent` in
 * the table means nothing to an operator who has never read the collector.
 */
const METRIC_LABELS: Record<string, string> = {
  'node.fd_percent': 'System file descriptors in use',
  'node.process_fd_percent': 'Process file descriptors in use',
  'proxy.error_rate_percent': 'Proxy 5xx rate',
  'proxy.request_duration_p99_ms': 'Proxy p99 latency',
}

/** Unit suffix per metric, so "90" reads as "90%" rather than a bare number. */
const METRIC_UNITS: Record<string, string> = {
  'node.fd_percent': '%',
  'node.process_fd_percent': '%',
  'proxy.error_rate_percent': '%',
  'proxy.request_duration_p99_ms': 'ms',
}

function metricLabel(metric: string): string {
  return METRIC_LABELS[metric] ?? metric
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
  isSaving: boolean
}

function RuleRow({ rule, onThresholdSave, onToggle, isSaving }: RuleRowProps) {
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
    </TableRow>
  )
}

export function NodeAlertRules() {
  const queryClient = useQueryClient()

  const rulesQuery = useQuery({
    ...nodeMetricsGetAlertRulesOptions({
      path: { id: CONTROL_PLANE_NODE_ID },
    }),
    staleTime: 30_000,
  })

  const updateMutation = useMutation({
    ...nodeMetricsUpdateAlertRuleMutation(),
    meta: { errorTitle: 'Failed to update the alert rule' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        predicate: (query) =>
          (query.queryKey[0] as Record<string, unknown>)?._id ===
          'nodeMetricsGetAlertRules',
      })
    },
  })

  const handleThresholdSave = (
    rule: ServiceAlertRuleResponse,
    threshold: number
  ) => {
    updateMutation.mutate(
      {
        path: { id: CONTROL_PLANE_NODE_ID, rule_id: rule.id },
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
        path: { id: CONTROL_PLANE_NODE_ID, rule_id: rule.id },
        body: { enabled },
      },
      {
        onSuccess: () =>
          toast.success(`${rule.name} ${enabled ? 'enabled' : 'disabled'}`),
      }
    )
  }

  const rules = rulesQuery.data ?? []

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="text-base">Platform alert rules</CardTitle>
        <CardDescription>
          Alerts the control-plane raises about itself — proxy health and file
          descriptor (socket) exhaustion. Seeded on startup; retune a threshold
          or switch a rule off here. Deleting is deliberately not offered: the
          defaults are re-seeded on every restart, so only disabling sticks.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {rulesQuery.isPending ? (
          <div className="space-y-2">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : rulesQuery.isError ? (
          <p className="px-2 py-6 text-center text-sm text-muted-foreground">
            Failed to load the platform alert rules.{' '}
            <button
              type="button"
              className="underline underline-offset-2"
              onClick={() => rulesQuery.refetch()}
            >
              Retry
            </button>
          </p>
        ) : rules.length === 0 ? (
          <p className="px-2 py-6 text-center text-sm text-muted-foreground">
            No alert rules on the control-plane node yet. Defaults are seeded
            the next time the server starts.
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
                </TableRow>
              </TableHeader>
              <TableBody>
                {rules.map((rule) => (
                  <RuleRow
                    key={rule.id}
                    rule={rule}
                    onThresholdSave={handleThresholdSave}
                    onToggle={handleToggle}
                    isSaving={updateMutation.isPending}
                  />
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
