/**
 * MonitoringCard — per-service metrics, live stats, chart, and alert-rule management.
 *
 * All API calls go through the generated OpenAPI SDK (`@/api/client`).
 */

import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { EmptyPlaceholder } from '@/components/EmptyPlaceholder'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  externalServiceMetricsStatusOptions,
  externalServiceMetricsGetLatestOptions,
  externalServiceMetricsGetLatestQueryKey,
  externalServiceMetricsGetRangeOptions,
  externalServiceMetricsGetAlertRulesOptions,
  externalServiceMetricsGetAlertRulesQueryKey,
  externalServiceMetricsCreateAlertRuleMutation,
  externalServiceMetricsUpdateAlertRuleMutation,
  externalServiceMetricsDeleteAlertRuleMutation,
  externalServiceMetricsToggleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ServiceAlertRuleResponse } from '@/api/client/types.gen'
import {
  Activity,
  AlertTriangle,
  ChevronDown,
  Loader2,
  Plus,
  PowerOff,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
} from 'recharts'
import { formatBytes } from '@/lib/utils'

// ---------------------------------------------------------------------------
// View-model types (derived from the generated SDK responses)
// ---------------------------------------------------------------------------

/** A single metric reading — normalised from the `{ name: value }` map the
 *  `metrics/latest` endpoint returns (e.g. { "pg.connections_active": 12.0 }). */
type MetricLatest = {
  name: string
  value: number
}

/** Alert-rule form-state unions. The API accepts `comparator`/`severity` as
 *  plain strings; these constrain the UI selects to the supported values. */
type Comparator = 'gt' | 'lt' | 'gte' | 'lte'
type Severity = 'info' | 'warning' | 'critical'

/** Map the human range selector (hours) to the API's `range` query value. */
const HOURS_TO_RANGE: Record<number, string> = {
  1: '1h',
  6: '6h',
  24: '24h',
  168: '7d',
}

/** Extract a comparable message from whatever the SDK throws on a failed
 *  request. `@hey-api/client-fetch` throws the parsed RFC 7807 Problem body
 *  ({ detail, title, status }), so we surface the same string the old
 *  hand-rolled fetch helper produced for the "monitoring not enabled" checks. */
function metricsErrorText(err: unknown): string {
  if (err == null) return ''
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  const problem = err as { detail?: string; title?: string; status?: number }
  return problem.detail ?? problem.title ?? `HTTP ${problem.status ?? ''}`
}

/** Whether an error means metrics are permanently unavailable for this service
 *  (disabled / not found / store offline) and polling should stop. */
function isMonitoringUnavailable(err: unknown): boolean {
  const msg = metricsErrorText(err).toLowerCase()
  return (
    msg.includes('not enabled') ||
    msg.includes('not found') ||
    msg.includes('unavailable') ||
    msg.includes('http 404') ||
    msg.includes('http 503')
  )
}

// ---------------------------------------------------------------------------
// Metric configuration per engine
// ---------------------------------------------------------------------------

type EngineKind = 'postgres' | 'redis' | 'mongodb' | 's3' | 'rustfs'

const ENGINE_STAT_METRICS: Record<EngineKind, string[]> = {
  postgres: [
    'pg.connections',
    'pg.cache_hit_ratio',
    'pg.replication_replay_lag_seconds',
    'pg.deadlocks_total',
  ],
  redis: [
    'redis.memory_used_bytes',
    'redis.keyspace_hit_ratio',
    'redis.evicted_keys_total',
    'redis.connected_clients',
  ],
  mongodb: [
    'mongo.connections_current',
    'mongo.wiredtiger_cache_ratio',
    'mongo.op_query_total',
    'mongo.replication_buffer_ratio',
  ],
  s3: ['s3.bucket_count', 's3.total_size_bytes', 's3.capacity_usable_total_bytes', 's3.object_count'],
  rustfs: ['rustfs_cluster_buckets_total', 'rustfs_cluster_capacity_usable_total_bytes', 'rustfs_cluster_capacity_used_bytes', 'rustfs_cluster_objects_total'],
}

const DEFAULT_CHART_METRIC: Record<EngineKind, string> = {
  postgres: 'pg.connections',
  redis: 'redis.connected_clients',
  mongodb: 'mongo.connections_current',
  s3: 's3.bucket_count',
  rustfs: 'rustfs_cluster_capacity_used_bytes',
}

const KNOWN_METRICS: Record<EngineKind, string[]> = {
  postgres: [
    'pg.connections',
    'pg.connections_active',
    'pg.connections_idle',
    'pg.connections_idle_in_transaction',
    'pg.queries_long_running',
    'pg.queries_blocked',
    'pg.locks_waiting',
    'pg.cache_hit_ratio',
    'pg.tuple_fetch_ratio',
    'pg.deadlocks_total',
    'pg.commits_total',
    'pg.rollbacks_total',
    'pg.temp_files_total',
    'pg.temp_bytes_total',
    'pg.tuples_inserted_total',
    'pg.tuples_updated_total',
    'pg.tuples_deleted_total',
    'pg.dead_tuple_ratio',
    'pg.replication_replay_lag_seconds',
    'pg.replication_write_lag_seconds',
    'pg.wal_bytes_total',
    'pg.wal_fpi_total',
    'pg.checkpoints_timed_total',
    'pg.checkpoints_req_total',
    'pg.database_size_bytes',
  ],
  redis: [
    'redis.connected_clients',
    'redis.blocked_clients',
    'redis.memory_used_bytes',
    'redis.memory_peak_bytes',
    'redis.memory_fragmentation_ratio',
    'redis.keyspace_hit_ratio',
    'redis.keyspace_hits_total',
    'redis.keyspace_misses_total',
    'redis.evicted_keys_total',
    'redis.expired_keys_total',
    'redis.ops_per_second',
    'redis.commands_processed_total',
    'redis.connections_received_total',
    'redis.net_input_bytes_total',
    'redis.net_output_bytes_total',
    'redis.rdb_last_save_duration_ms',
    'redis.replication_offset_lag',
  ],
  mongodb: [
    'mongo.connections_current',
    'mongo.connections_available',
    'mongo.connections_total_created',
    'mongo.op_insert_total',
    'mongo.op_query_total',
    'mongo.op_update_total',
    'mongo.op_delete_total',
    'mongo.op_getmore_total',
    'mongo.op_command_total',
    'mongo.network_bytes_in_total',
    'mongo.network_bytes_out_total',
    'mongo.network_requests_total',
    'mongo.active_reads',
    'mongo.active_writes',
    'mongo.queued_reads',
    'mongo.queued_writes',
    'mongo.wiredtiger_cache_ratio',
    'mongo.wiredtiger_cache_dirty_ratio',
    'mongo.wiredtiger_cache_bytes_used',
    'mongo.wiredtiger_cache_bytes_max',
    'mongo.wiredtiger_evicted_pages_total',
    'mongo.document_inserted_total',
    'mongo.document_returned_total',
    'mongo.document_updated_total',
    'mongo.document_deleted_total',
    'mongo.cursor_open_total',
    'mongo.cursor_timed_out_total',
    'mongo.replication_buffer_ratio',
  ],
  s3: [
    's3.bucket_count',
    's3.total_size_bytes',
    's3.capacity_usable_total_bytes',
    's3.capacity_usable_free_bytes',
    's3.object_count',
    's3.nodes_online',
    's3.nodes_offline',
  ],
  rustfs: [
    'rustfs_cluster_buckets_total',
    'rustfs_cluster_objects_total',
    'rustfs_cluster_capacity_usable_total_bytes',
    'rustfs_cluster_capacity_used_bytes',
    'rustfs_cluster_capacity_free_bytes',
    'rustfs_cluster_capacity_raw_total_bytes',
    'rustfs_process_memory_bytes',
    'rustfs_process_cpu_percent',
    'rustfs_process_uptime_seconds',
    'rustfs_s3_operations_total',
    'rustfs.api.requests.total',
    'rustfs.request.body.bytes_total',
    'rustfs_node_disk_free_bytes',
    'rustfs_node_disk_total_bytes',
    'rustfs_node_disk_used_bytes',
    'rustfs_system_process_cpu_usage',
    'rustfs_system_process_resident_memory_bytes',
    's3.bucket_count',
  ],
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function isNormalEngine(engine: string): engine is EngineKind {
  return ['postgres', 'redis', 'mongodb', 's3', 'rustfs'].includes(engine)
}

function normalizeEngine(engine: string, dockerImage?: string): EngineKind {
  if (engine === 's3' && dockerImage?.toLowerCase().includes('rustfs')) return 'rustfs'
  return isNormalEngine(engine) ? engine : 'postgres'
}

/** Compact relative time, e.g. "just now", "12s ago", "3m ago", "2h ago". */
function formatRelativeTime(iso: string): string {
  const then = new Date(iso).getTime()
  if (Number.isNaN(then)) return ''
  const secs = Math.max(0, Math.floor((Date.now() - then) / 1000))
  if (secs < 5) return 'just now'
  if (secs < 60) return `${secs}s ago`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ago`
  const hours = Math.floor(mins / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

function formatMetricValue(name: string, value: number): string {
  if (name.endsWith('_bytes')) return formatBytes(value)
  if (name.endsWith('_ratio')) return `${(value * 100).toFixed(1)}%`
  if (name.endsWith('_percent')) return `${value.toFixed(1)}%`
  if (name.endsWith('_seconds') || name.endsWith('_sec'))
    return `${value.toFixed(2)}s`
  if (name.endsWith('_ms')) return `${value.toFixed(0)}ms`
  // Counter/gauge metrics that represent whole numbers (aggregated avg may be float)
  if (
    name.endsWith('_active') ||
    name.endsWith('_total') ||
    name.endsWith('_count') ||
    name.endsWith('_clients') ||
    name.endsWith('_connections')
  )
    return Math.round(value).toString()
  if (Number.isInteger(value)) return value.toString()
  return value.toFixed(2)
}

function labelForMetric(name: string): string {
  // Strip engine prefix (e.g. "pg.", "redis.", "mongo.", "s3.")
  const bare = name.replace(/^[a-z0-9]+\./, '')
  return bare
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase())
}

// ---------------------------------------------------------------------------
// Range options
// ---------------------------------------------------------------------------

type RangeOption = { label: string; hours: number }

const RANGE_OPTIONS: RangeOption[] = [
  { label: '1h', hours: 1 },
  { label: '6h', hours: 6 },
  { label: '24h', hours: 24 },
  { label: '7d', hours: 168 },
]

// ---------------------------------------------------------------------------
// AddAlertRuleDialog
// ---------------------------------------------------------------------------

type AddAlertRuleDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
  serviceId: number
  engine: EngineKind
  onSuccess: () => void
}

function AddAlertRuleDialog({
  open,
  onOpenChange,
  serviceId,
  engine,
  onSuccess,
}: AddAlertRuleDialogProps) {
  const [name, setName] = useState('')
  const [metricName, setMetricName] = useState(KNOWN_METRICS[engine][0] ?? '')
  const [threshold, setThreshold] = useState('0')
  const [comparator, setComparator] = useState<Comparator>('gt')
  const [severity, setSeverity] = useState<Severity>('warning')

  const create = useMutation({
    ...externalServiceMetricsCreateAlertRuleMutation(),
    onSuccess: () => {
      toast.success('Alert rule created')
      onSuccess()
      onOpenChange(false)
      setName('')
      setThreshold('0')
    },
    onError: (err: Error) =>
      toast.error('Failed to create alert rule', { description: err.message }),
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add Alert Rule</DialogTitle>
          <DialogDescription>
            Fire an alarm when the chosen metric crosses the threshold.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4 py-2">
          <div className="space-y-1.5">
            <label className="text-sm font-medium">Rule name</label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. High connection count"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium">Metric</label>
            <Select value={metricName} onValueChange={setMetricName}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {KNOWN_METRICS[engine].map((m) => (
                  <SelectItem key={m} value={m}>
                    {labelForMetric(m)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex gap-3">
            <div className="space-y-1.5 w-28">
              <label className="text-sm font-medium">Comparator</label>
              <Select
                value={comparator}
                onValueChange={(v) => setComparator(v as Comparator)}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="gt">&gt; greater than</SelectItem>
                  <SelectItem value="gte">&ge; greater or equal</SelectItem>
                  <SelectItem value="lt">&lt; less than</SelectItem>
                  <SelectItem value="lte">&le; less or equal</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="flex-1 space-y-1.5">
              <label className="text-sm font-medium">Threshold</label>
              <Input
                type="number"
                value={threshold}
                onChange={(e) => setThreshold(e.target.value)}
              />
            </div>
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium">Severity</label>
            <Select
              value={severity}
              onValueChange={(v) => setSeverity(v as Severity)}
            >
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="info">Info</SelectItem>
                <SelectItem value="warning">Warning</SelectItem>
                <SelectItem value="critical">Critical</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            onClick={() =>
              create.mutate({
                path: { id: serviceId },
                body: {
                  name,
                  metric_name: metricName,
                  comparator,
                  threshold: parseFloat(threshold),
                  severity,
                },
              })
            }
            disabled={create.isPending || !name.trim()}
          >
            {create.isPending && (
              <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            )}
            Add Rule
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// AlertRulesSection (collapsible)
// ---------------------------------------------------------------------------

type AlertRulesSectionProps = {
  serviceId: number
  engine: EngineKind
}

function AlertRulesSection({ serviceId, engine }: AlertRulesSectionProps) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [ruleToDelete, setRuleToDelete] = useState<ServiceAlertRuleResponse | null>(null)
  // Track which rule is being threshold-edited inline: ruleId -> string value
  const [editingThreshold, setEditingThreshold] = useState<
    Record<number, string>
  >({})

  const { data: rules, isLoading } = useQuery({
    ...externalServiceMetricsGetAlertRulesOptions({ path: { id: serviceId } }),
    enabled: open,
  })

  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: externalServiceMetricsGetAlertRulesQueryKey({
        path: { id: serviceId },
      }),
    })

  const updateRule = useMutation({
    ...externalServiceMetricsUpdateAlertRuleMutation(),
    onSuccess: () => invalidate(),
    onError: (err: Error) =>
      toast.error('Failed to update rule', { description: err.message }),
  })

  const removeRule = useMutation({
    ...externalServiceMetricsDeleteAlertRuleMutation(),
    onSuccess: () => {
      toast.success('Alert rule deleted')
      setRuleToDelete(null)
      invalidate()
    },
    onError: (err: Error) =>
      toast.error('Failed to delete rule', { description: err.message }),
  })

  return (
    <>
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            className="w-full justify-between px-0 text-sm font-medium hover:bg-transparent"
          >
            Alert Rules
            <ChevronDown
              className={`h-4 w-4 text-muted-foreground transition-transform ${open ? 'rotate-180' : ''}`}
            />
          </Button>
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="mt-3 space-y-3">
            {isLoading ? (
              <div className="flex items-center gap-2 py-2 text-sm text-muted-foreground">
                <Loader2 className="h-3 w-3 animate-spin" />
                Loading rules…
              </div>
            ) : !rules || rules.length === 0 ? (
              <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-8 text-center">
                <Activity className="h-6 w-6 text-muted-foreground" />
                <p className="max-w-xs text-sm text-muted-foreground">
                  No alert rules yet. Add one to get notified when metrics cross a
                  threshold.
                </p>
              </div>
            ) : (
              <div className="overflow-x-auto -mx-1 px-1">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Name</TableHead>
                      <TableHead className="hidden sm:table-cell">Metric</TableHead>
                      <TableHead>Threshold</TableHead>
                      <TableHead className="hidden sm:table-cell">Severity</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="w-8" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {rules.map((rule) => {
                      const editing = editingThreshold[rule.id]
                      return (
                        <TableRow key={rule.id} className="even:bg-muted/30">
                          <TableCell className="font-medium text-foreground">
                            {rule.name}
                          </TableCell>
                          <TableCell className="hidden sm:table-cell font-mono text-xs text-muted-foreground">
                            {rule.metric_name}
                          </TableCell>
                          <TableCell>
                            {editing !== undefined ? (
                              <Input
                                type="number"
                                value={editing}
                                className="h-7 w-24 px-2 text-sm"
                                autoFocus
                                onChange={(e) =>
                                  setEditingThreshold((prev) => ({
                                    ...prev,
                                    [rule.id]: e.target.value,
                                  }))
                                }
                                onBlur={() => {
                                  const val = parseFloat(editing)
                                  if (!isNaN(val) && val !== rule.threshold) {
                                    updateRule.mutate({
                                      path: { id: serviceId, rule_id: rule.id },
                                      body: { threshold: val },
                                    })
                                  }
                                  setEditingThreshold((prev) => {
                                    const next = { ...prev }
                                    delete next[rule.id]
                                    return next
                                  })
                                }}
                                onKeyDown={(e) => {
                                  if (e.key === 'Enter') e.currentTarget.blur()
                                  if (e.key === 'Escape') {
                                    setEditingThreshold((prev) => {
                                      const next = { ...prev }
                                      delete next[rule.id]
                                      return next
                                    })
                                  }
                                }}
                              />
                            ) : (
                              <button
                                type="button"
                                className="rounded px-1.5 py-0.5 text-sm tabular-nums text-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                                title="Click to edit"
                                onClick={() =>
                                  setEditingThreshold((prev) => ({
                                    ...prev,
                                    [rule.id]: String(rule.threshold),
                                  }))
                                }
                              >
                                {rule.comparator} {rule.threshold}
                              </button>
                            )}
                          </TableCell>
                          <TableCell className="hidden sm:table-cell">
                            <Badge
                              variant={
                                rule.severity === 'critical'
                                  ? 'destructive'
                                  : rule.severity === 'warning'
                                    ? 'outline'
                                    : 'secondary'
                              }
                              className="capitalize text-xs"
                            >
                              {rule.severity}
                            </Badge>
                          </TableCell>
                          <TableCell>
                            <Badge variant="outline" className="text-xs">
                              OK
                            </Badge>
                          </TableCell>
                          <TableCell>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="h-7 w-7 text-muted-foreground hover:text-destructive"
                              aria-label={`Delete rule ${rule.name}`}
                              onClick={() => setRuleToDelete(rule)}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </TableCell>
                        </TableRow>
                      )
                    })}
                  </TableBody>
                </Table>
              </div>
            )}
            <Button
              variant="outline"
              size="sm"
              className="gap-2"
              onClick={() => setAddDialogOpen(true)}
            >
              <Plus className="h-4 w-4" />
              Add Rule
            </Button>
          </div>
        </CollapsibleContent>
      </Collapsible>

      <AddAlertRuleDialog
        open={addDialogOpen}
        onOpenChange={setAddDialogOpen}
        serviceId={serviceId}
        engine={engine}
        onSuccess={invalidate}
      />

      {/* Delete confirm dialog */}
      <Dialog
        open={!!ruleToDelete}
        onOpenChange={(o) => {
          if (!o) setRuleToDelete(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Alert Rule</DialogTitle>
            <DialogDescription>
              Delete the rule{' '}
              <span className="font-medium text-foreground">
                {ruleToDelete?.name}
              </span>
              ? This cannot be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setRuleToDelete(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={removeRule.isPending}
              onClick={() =>
                ruleToDelete &&
                removeRule.mutate({
                  path: { id: serviceId, rule_id: ruleToDelete.id },
                })
              }
            >
              {removeRule.isPending && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}

// ---------------------------------------------------------------------------
// LiveMetrics (State C)
// ---------------------------------------------------------------------------

type LiveMetricsProps = {
  serviceId: number
  engine: EngineKind
  latestMetrics: MetricLatest[]
}

// --chart-1 is oklch(0.59 0.2032 256.82) = #0070f3 in globals.css.
// SVG stroke doesn't understand oklch(), so we use the hex value directly.
// This matches the design token — not an arbitrary choice.
const CHART_LINE_COLOR = '#0070f3'

function LiveMetrics({ serviceId, engine, latestMetrics }: LiveMetricsProps) {
  const statMetrics = ENGINE_STAT_METRICS[engine]
  const [selectedMetric, setSelectedMetric] = useState(
    DEFAULT_CHART_METRIC[engine],
  )
  const [rangeHours, setRangeHours] = useState(1)

  // Build a lookup from the latest array
  const latestByName = new Map<string, MetricLatest>()
  for (const m of latestMetrics) {
    latestByName.set(m.name, m)
  }

  // Keep the alert rules warm so the section opens instantly; the backend does
  // not surface a per-rule firing state, so the badge count stays at zero.
  useQuery({
    ...externalServiceMetricsGetAlertRulesOptions({ path: { id: serviceId } }),
    staleTime: 30_000,
    refetchInterval: 30_000,
  })

  const firingCount = 0

  // Range chart data
  const rangeLabel = HOURS_TO_RANGE[rangeHours] ?? '1h'
  const { data: rangeData, isLoading: rangeLoading } = useQuery({
    ...externalServiceMetricsGetRangeOptions({
      path: { id: serviceId },
      query: { metric: selectedMetric, range: rangeLabel },
    }),
    staleTime: 15_000,
    refetchInterval: 30_000,
  })

  const chartData = (rangeData ?? []).map((p) => ({
    time: new Date(p.time).toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
    }),
    value: p.value,
  }))

  return (
    <div className="space-y-4 overflow-visible">
      {/* Active alerts badge */}
      {firingCount > 0 && (
        <div className="flex items-center gap-2 rounded-md bg-destructive/10 px-3 py-2 text-sm text-destructive ring-1 ring-inset ring-destructive/20">
          <AlertTriangle className="h-4 w-4 flex-shrink-0" />
          <span className="font-medium">
            {firingCount} active alert{firingCount > 1 ? 's' : ''}
          </span>
          <Link
            to="/monitoring/alarms"
            className="ml-auto text-xs underline underline-offset-2 hover:no-underline"
          >
            View
          </Link>
        </div>
      )}

      {/* Stat row */}
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        {statMetrics.map((name) => {
          const latest = latestByName.get(name)
          const isSelected = name === selectedMetric
          return (
            <button
              key={name}
              type="button"
              onClick={() => setSelectedMetric(name)}
              className={`flex flex-col gap-0.5 rounded-md border p-3 text-left transition-colors hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${
                isSelected
                  ? 'border-primary/40 bg-primary/5 ring-1 ring-inset ring-primary/30'
                  : 'border-border bg-card'
              }`}
            >
              <span className="text-[11px] font-medium text-muted-foreground">
                {labelForMetric(name)}
              </span>
              <span className="text-base font-semibold tabular-nums text-foreground">
                {latest != null
                  ? formatMetricValue(name, latest.value)
                  : '—'}
              </span>
            </button>
          )
        })}
      </div>

      {/* Range selector + chart */}
      <div className="space-y-2 overflow-visible">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-sm font-medium">{labelForMetric(selectedMetric)}</span>
          <div className="flex gap-1">
            {RANGE_OPTIONS.map((opt) => (
              <Button
                key={opt.hours}
                variant={rangeHours === opt.hours ? 'default' : 'outline'}
                size="sm"
                className="h-7 px-2.5 text-xs"
                onClick={() => setRangeHours(opt.hours)}
              >
                {opt.label}
              </Button>
            ))}
          </div>
        </div>
        <div className="relative z-20 h-48 w-full overflow-visible">
          {rangeLoading ? (
            <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin mr-2" />
              Loading…
            </div>
          ) : chartData.length === 0 ? (
            <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
              No data for this range
            </div>
          ) : (
            <ResponsiveContainer width="100%" height="100%">
              <LineChart
                data={chartData}
                margin={{ top: 4, right: 16, left: 0, bottom: 0 }}
              >
                <XAxis
                  dataKey="time"
                  tick={{ fontSize: 10, fill: 'rgba(156,163,175,0.9)' }}
                  tickLine={false}
                  axisLine={false}
                  interval="preserveStartEnd"
                />
                <YAxis
                  tick={{ fontSize: 10, fill: 'rgba(156,163,175,0.9)' }}
                  tickLine={false}
                  axisLine={false}
                  width={
                    selectedMetric.endsWith('_ratio') ||
                    selectedMetric.endsWith('_percent')
                      ? 56
                      : selectedMetric.endsWith('_bytes')
                        ? 60
                        : 44
                  }
                  tickFormatter={(v: number) =>
                    formatMetricValue(selectedMetric, v)
                  }
                />
                <Tooltip
                  wrapperStyle={{ zIndex: 50 }}
                  allowEscapeViewBox={{ x: true, y: true }}
                  contentStyle={{
                    fontSize: 12,
                    backgroundColor: 'hsl(var(--popover))',
                    border: '1px solid hsl(var(--border))',
                    borderRadius: '6px',
                    color: 'hsl(var(--popover-foreground))',
                    boxShadow: '0 4px 6px -1px rgb(0 0 0 / 0.3)',
                    padding: '6px 10px',
                  }}
                  labelStyle={{
                    color: 'hsl(var(--muted-foreground))',
                    fontSize: 11,
                    marginBottom: 2,
                  }}
                  itemStyle={{ color: CHART_LINE_COLOR }}
                  cursor={{ stroke: 'rgba(128,128,128,0.3)', strokeWidth: 1 }}
                  formatter={(v: number) => [
                    formatMetricValue(selectedMetric, v),
                    labelForMetric(selectedMetric),
                  ]}
                />
                <Line
                  type="monotone"
                  dataKey="value"
                  dot={false}
                  strokeWidth={2}
                  stroke={CHART_LINE_COLOR}
                  isAnimationActive={false}
                />
              </LineChart>
            </ResponsiveContainer>
          )}
        </div>
      </div>

      {/* Alert rules collapsible */}
      <AlertRulesSection serviceId={serviceId} engine={engine} />
    </div>
  )
}

// ---------------------------------------------------------------------------
// MonitoringCard (public export)
// ---------------------------------------------------------------------------

export interface MonitoringCardProps {
  serviceId: number
  /** 'postgres' | 'redis' | 'mongodb' | 's3' | 'rustfs' */
  engine: string
  /** Docker image — used to detect RustFS when service_type is 's3' */
  dockerImage?: string
  /** Whether metric collection is enabled for this service. When false, the
   *  card shows the "Enable Monitoring" state and does NOT poll the API. */
  metricsEnabled?: boolean
  /** Called after monitoring is toggled on/off so the parent can refetch the
   *  service (which owns the `metricsEnabled` flag). */
  onMonitoringChange?: () => void
}

/**
 * Three render states:
 *   A — metrics disabled → EmptyPlaceholder with Enable button
 *   B — enabled, no data yet → pending card with polling
 *   C — data available → live stat row + chart + alert rules
 */
export function MonitoringCard({ serviceId, engine, dockerImage, metricsEnabled = false, onMonitoringChange }: MonitoringCardProps) {
  const queryClient = useQueryClient()
  const normalEngine = normalizeEngine(engine, dockerImage)

  // Poll /latest every 3s. If the result is null/empty → State B. If data → State C.
  // HTTP 404 from the backend means metrics are not enabled → State A.
  const {
    data: latestMetrics,
    isLoading,
    error,
  } = useQuery({
    ...externalServiceMetricsGetLatestOptions({ path: { id: serviceId } }),
    // Normalise the `{ name: value }` map the endpoint returns into the array
    // shape the rest of the card consumes.
    select: (map): MetricLatest[] =>
      Object.entries(map).map(([name, value]) => ({ name, value })),
    // Only hit the API when monitoring is enabled for this service. When off,
    // we render the Enable state from the prop without any network calls.
    enabled: metricsEnabled,
    retry: (failureCount, err) => {
      // Don't retry on permanent errors (disabled, not found)
      if (isMonitoringUnavailable(err)) return false
      return failureCount < 2
    },
    staleTime: 3_000,
    refetchInterval: (query) => {
      // Stop polling entirely on permanent errors (monitoring disabled on the
      // server or this service, service not found). Otherwise we'd hammer the
      // API every 3s forever and the card would visibly blink.
      if (query.state.error && isMonitoringUnavailable(query.state.error)) {
        return false
      }
      // Stop fast-polling once we have data (switch to 30s passive refresh)
      const d = query.state.data
      if (d && Object.keys(d).length > 0) return 30_000
      // Poll every 3s while waiting for the first scrape after enabling
      return 3_000
    },
  })

  // Freshness status — when metrics were last received (cheap O(1) lookup).
  const { data: statusData } = useQuery({
    ...externalServiceMetricsStatusOptions({ path: { id: serviceId } }),
    enabled: metricsEnabled,
    refetchInterval: 30_000,
  })
  const lastReceivedAt = statusData?.last_received_at ?? null

  const enableMonitoring = useMutation({
    ...externalServiceMetricsToggleMutation(),
    onSuccess: () => {
      toast.success('Monitoring enabled — first metrics appear within 30 seconds.')
      queryClient.invalidateQueries({
        queryKey: externalServiceMetricsGetLatestQueryKey({
          path: { id: serviceId },
        }),
      })
      // Refetch the parent service so the metricsEnabled flag flips to true and
      // the query un-gates.
      onMonitoringChange?.()
    },
    onError: (err: Error) =>
      toast.error('Failed to enable monitoring', { description: err.message }),
  })

  const disableMonitoring = useMutation({
    ...externalServiceMetricsToggleMutation(),
    onSuccess: () => {
      toast.success('Monitoring disabled.')
      queryClient.invalidateQueries({
        queryKey: externalServiceMetricsGetLatestQueryKey({
          path: { id: serviceId },
        }),
      })
      onMonitoringChange?.()
    },
    onError: (err: Error) =>
      toast.error('Failed to disable monitoring', { description: err.message }),
  })

  // State A: monitoring disabled for this service. Either the service flag is
  // off (no API call made at all) or the API reported it's not enabled.
  const isDisabled = !metricsEnabled || (error != null && isMonitoringUnavailable(error))

  if (isDisabled) {
    return (
      <EmptyPlaceholder
        icon={Activity}
        title="Database Monitoring"
        description="Performance metrics, query insights, and historical trends."
      >
        <Button
          onClick={() =>
            enableMonitoring.mutate({
              path: { id: serviceId },
              body: { enabled: true },
            })
          }
          disabled={enableMonitoring.isPending}
        >
          {enableMonitoring.isPending && (
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
          )}
          Enable Monitoring
        </Button>
      </EmptyPlaceholder>
    )
  }

  // Still loading for the first time
  if (isLoading && !latestMetrics) {
    return (
      <Card className="dark:shadow-none">
        <CardContent className="flex items-center gap-3 py-6">
          <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
          <span className="text-sm text-muted-foreground">
            Loading metrics…
          </span>
        </CardContent>
      </Card>
    )
  }

  // State B: enabled but no data yet (empty array)
  if (!latestMetrics || latestMetrics.length === 0) {
    return (
      <Card>
        <CardContent className="flex items-center justify-between py-6">
          <div className="flex items-center gap-3">
            <span className="relative flex h-3 w-3">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-primary opacity-75" />
              <span className="relative inline-flex h-3 w-3 rounded-full bg-primary" />
            </span>
            <div>
              <p className="text-sm font-medium">Setting up metrics collection…</p>
              <p className="text-muted-foreground text-sm">
                First metrics appear within 30 seconds.
              </p>
            </div>
          </div>
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              disableMonitoring.mutate({
                path: { id: serviceId },
                body: { enabled: false },
              })
            }
            disabled={disableMonitoring.isPending}
            className="text-muted-foreground hover:text-destructive"
          >
            {disableMonitoring.isPending
              ? <Loader2 className="h-4 w-4 animate-spin" />
              : <PowerOff className="h-4 w-4" />}
            <span className="ml-1.5 hidden sm:inline">Disable</span>
          </Button>
        </CardContent>
      </Card>
    )
  }

  // State C: live metrics
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="flex items-center justify-between text-base">
          <span className="flex items-center gap-2">
            <Activity className="h-4 w-4" />
            Monitoring
            {lastReceivedAt && (
              <span className="text-xs font-normal text-muted-foreground">
                · last received {formatRelativeTime(lastReceivedAt)}
              </span>
            )}
          </span>
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              disableMonitoring.mutate({
                path: { id: serviceId },
                body: { enabled: false },
              })
            }
            disabled={disableMonitoring.isPending}
            className="text-muted-foreground hover:text-destructive"
          >
            {disableMonitoring.isPending
              ? <Loader2 className="h-4 w-4 animate-spin" />
              : <PowerOff className="h-4 w-4" />}
            <span className="ml-1.5 hidden sm:inline">Disable</span>
          </Button>
        </CardTitle>
      </CardHeader>
      <CardContent className="overflow-visible">
        <LiveMetrics
          serviceId={serviceId}
          engine={normalEngine}
          latestMetrics={latestMetrics}
        />
      </CardContent>
    </Card>
  )
}
