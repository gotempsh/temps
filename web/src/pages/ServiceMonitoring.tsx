/**
 * ServiceMonitoring — full-page metrics dashboard for a single external service.
 *
 * Route: /storage/:id/monitoring
 *
 * Sections (per engine):
 *   - Hero stat row: key headline numbers
 *   - Categorised metric grids (connections, performance, activity, storage, …)
 *   - Multi-chart panel: click any metric card to inspect its time-series
 *   - Alert rules section
 */

import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
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
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getServiceOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  Activity,
  ArrowLeft,
  Loader2,
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
} from 'recharts'
import { formatBytes } from '@/lib/utils'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type MetricLatest = { name: string; value: number }
type MetricRangePoint = { time: string; value: number }

type ServiceAlertRule = {
  id: number
  name: string
  metric_name: string
  comparator: 'gt' | 'lt' | 'gte' | 'lte'
  threshold: number
  severity: 'info' | 'warning' | 'critical'
  for_duration_secs: number
  enabled: boolean
  status: 'firing' | 'ok'
}

type ServiceAlertRuleCreateRequest = {
  name: string
  metric_name: string
  comparator: 'gt' | 'lt' | 'gte' | 'lte'
  threshold: number
  severity: 'info' | 'warning' | 'critical'
  for_duration_secs?: number
  enabled?: boolean
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

async function fetchLatestMetrics(serviceId: number): Promise<MetricLatest[]> {
  const res = await fetch(`/api/external-services/${serviceId}/metrics/latest`, {
    credentials: 'include',
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({})) as { detail?: string; title?: string }
    throw new Error(body.detail ?? body.title ?? `HTTP ${res.status}`)
  }
  const map = await res.json() as Record<string, number>
  return Object.entries(map).map(([name, value]) => ({ name, value }))
}

async function fetchMetricRange(
  serviceId: number,
  metricName: string,
  range: string,
): Promise<MetricRangePoint[]> {
  const res = await fetch(
    `/api/external-services/${serviceId}/metrics?metric=${encodeURIComponent(metricName)}&range=${range}`,
    { credentials: 'include' },
  )
  if (!res.ok) throw new Error('Failed to fetch metric range')
  return res.json() as Promise<MetricRangePoint[]>
}

async function fetchAlertRules(serviceId: number): Promise<ServiceAlertRule[]> {
  const res = await fetch(`/api/external-services/${serviceId}/metrics/alert-rules`, {
    credentials: 'include',
  })
  if (!res.ok) throw new Error('Failed to fetch alert rules')
  return res.json() as Promise<ServiceAlertRule[]>
}

async function createAlertRule(
  serviceId: number,
  body: ServiceAlertRuleCreateRequest,
): Promise<ServiceAlertRule> {
  const res = await fetch(`/api/external-services/${serviceId}/metrics/alert-rules`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: JSON.stringify(body),
  })
  if (!res.ok) {
    const err = await res.json().catch(() => ({}))
    throw new Error((err as { detail?: string }).detail ?? 'Failed to create alert rule')
  }
  return res.json() as Promise<ServiceAlertRule>
}

async function deleteAlertRule(serviceId: number, ruleId: number): Promise<void> {
  const res = await fetch(
    `/api/external-services/${serviceId}/metrics/alert-rules/${ruleId}`,
    { method: 'DELETE', credentials: 'include' },
  )
  if (!res.ok) {
    const err = await res.json().catch(() => ({}))
    throw new Error((err as { detail?: string }).detail ?? 'Failed to delete alert rule')
  }
}

// ---------------------------------------------------------------------------
// Metric metadata per engine
// ---------------------------------------------------------------------------

type EngineKind = 'postgres' | 'redis' | 'mongodb' | 's3' | 'rustfs'

type MetricGroup = {
  title: string
  metrics: string[]
}

const ENGINE_GROUPS: Record<EngineKind, MetricGroup[]> = {
  postgres: [
    {
      title: 'Connections',
      metrics: [
        'pg.connections_active',
        'pg.connections_idle',
        'pg.connections_idle_in_transaction',
        'pg.connections_other',
        'pg.queries_long_running',
        'pg.queries_blocked',
        'pg.locks_waiting',
      ],
    },
    {
      title: 'Performance',
      metrics: [
        'pg.cache_hit_ratio',
        'pg.tuple_fetch_ratio',
        'pg.commits_total',
        'pg.rollbacks_total',
        'pg.deadlocks_total',
        'pg.temp_files_total',
        'pg.temp_bytes_total',
      ],
    },
    {
      title: 'Throughput',
      metrics: [
        'pg.tuples_inserted_total',
        'pg.tuples_updated_total',
        'pg.tuples_deleted_total',
        'pg.tuples_live',
        'pg.tuples_dead',
        'pg.dead_tuple_ratio',
        'pg.seq_scans_total',
        'pg.idx_scans_total',
      ],
    },
    {
      title: 'WAL',
      metrics: [
        'pg.wal_bytes_total',
        'pg.wal_records_total',
        'pg.wal_fpi_total',
        'pg.wal_buffers_full_total',
        'pg.checkpoints_timed_total',
        'pg.checkpoints_req_total',
        'pg.checkpoint_rate',
      ],
    },
    {
      title: 'Replication',
      metrics: [
        'pg.replication_write_lag_seconds',
        'pg.replication_replay_lag_seconds',
      ],
    },
    {
      title: 'Storage',
      metrics: ['pg.database_size_bytes'],
    },
  ],
  redis: [
    {
      title: 'Clients',
      metrics: ['redis.connected_clients', 'redis.blocked_clients'],
    },
    {
      title: 'Memory',
      metrics: [
        'redis.memory_used_bytes',
        'redis.memory_peak_bytes',
        'redis.memory_fragmentation_ratio',
      ],
    },
    {
      title: 'Cache',
      metrics: [
        'redis.keyspace_hit_ratio',
        'redis.keyspace_hits_total',
        'redis.keyspace_misses_total',
        'redis.evicted_keys_total',
        'redis.expired_keys_total',
      ],
    },
    {
      title: 'Operations',
      metrics: [
        'redis.ops_per_second',
        'redis.commands_processed_total',
        'redis.connections_received_total',
      ],
    },
    {
      title: 'Network',
      metrics: ['redis.net_input_bytes_total', 'redis.net_output_bytes_total'],
    },
    {
      title: 'Persistence',
      metrics: ['redis.rdb_last_save_duration_ms'],
    },
    {
      title: 'Replication',
      metrics: ['redis.replication_offset_lag'],
    },
  ],
  mongodb: [
    {
      title: 'Connections',
      metrics: ['mongo.connections_current', 'mongo.connections_available'],
    },
    {
      title: 'Operations',
      metrics: [
        'mongo.op_insert_total',
        'mongo.op_query_total',
        'mongo.op_update_total',
        'mongo.op_delete_total',
        'mongo.op_getmore_total',
        'mongo.op_command_total',
      ],
    },
    {
      title: 'Network',
      metrics: [
        'mongo.network_bytes_in_total',
        'mongo.network_bytes_out_total',
        'mongo.network_requests_total',
      ],
    },
    {
      title: 'Lock queue',
      metrics: [
        'mongo.active_reads',
        'mongo.active_writes',
        'mongo.queued_reads',
        'mongo.queued_writes',
      ],
    },
    {
      title: 'Documents',
      metrics: [
        'mongo.document_inserted_total',
        'mongo.document_returned_total',
        'mongo.document_updated_total',
        'mongo.document_deleted_total',
        'mongo.cursor_open_total',
        'mongo.cursor_timed_out_total',
      ],
    },
    {
      title: 'Cache',
      metrics: [
        'mongo.wiredtiger_cache_ratio',
        'mongo.wiredtiger_cache_dirty_ratio',
        'mongo.wiredtiger_cache_bytes_used',
        'mongo.wiredtiger_cache_bytes_max',
        'mongo.wiredtiger_evicted_pages_total',
      ],
    },
    {
      title: 'Replication',
      metrics: ['mongo.replication_buffer_ratio'],
    },
  ],
  s3: [
    {
      title: 'Storage',
      metrics: [
        's3.bucket_count',
        's3.object_count',
        's3.total_size_bytes',
        's3.capacity_usable_total_bytes',
        's3.capacity_usable_free_bytes',
      ],
    },
    {
      title: 'Cluster',
      metrics: ['s3.nodes_online', 's3.nodes_offline'],
    },
  ],
  rustfs: [
    {
      title: 'Storage',
      metrics: [
        'rustfs_cluster_buckets_total',
        'rustfs_cluster_objects_total',
        'rustfs_cluster_capacity_usable_total_bytes',
        'rustfs_cluster_capacity_used_bytes',
        'rustfs_cluster_capacity_free_bytes',
        'rustfs_cluster_capacity_raw_total_bytes',
        'rustfs_node_disk_total_bytes',
        'rustfs_node_disk_used_bytes',
        'rustfs_node_disk_free_bytes',
        's3.bucket_count',
      ],
    },
    {
      title: 'Operations',
      metrics: [
        'rustfs_s3_operations_total',
        'rustfs.api.requests.total',
        'rustfs.request.body.bytes_total',
      ],
    },
    {
      title: 'Process',
      metrics: [
        'rustfs_process_cpu_percent',
        'rustfs_process_memory_bytes',
        'rustfs_process_uptime_seconds',
        'rustfs_system_process_cpu_usage',
        'rustfs_system_process_resident_memory_bytes',
      ],
    },
  ],
}

// All known metrics for alert rule creation
const ALL_METRICS: Record<EngineKind, string[]> = Object.fromEntries(
  Object.entries(ENGINE_GROUPS).map(([engine, groups]) => [
    engine,
    groups.flatMap((g) => g.metrics),
  ]),
) as Record<EngineKind, string[]>

// Headline metrics shown in the hero row
const HERO_METRICS: Record<EngineKind, string[]> = {
  postgres: [
    'pg.connections_active',
    'pg.cache_hit_ratio',
    'pg.deadlocks_total',
    'pg.database_size_bytes',
  ],
  redis: [
    'redis.connected_clients',
    'redis.memory_used_bytes',
    'redis.keyspace_hit_ratio',
    'redis.evicted_keys_total',
  ],
  mongodb: [
    'mongo.connections_current',
    'mongo.wiredtiger_cache_ratio',
    'mongo.op_query_total',
    'mongo.replication_buffer_ratio',
  ],
  s3: ['s3.bucket_count', 's3.total_size_bytes', 's3.capacity_usable_total_bytes', 's3.object_count'],
  rustfs: ['rustfs_cluster_buckets_total', 'rustfs_cluster_capacity_used_bytes', 'rustfs_cluster_capacity_free_bytes', 'rustfs_s3_operations_total'],
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function normalizeEngine(engine: string, dockerImage?: string | null): EngineKind {
  // An s3 service running the rustfs image should use rustfs metric groups
  if (engine === 's3' && dockerImage?.toLowerCase().includes('rustfs')) return 'rustfs'
  if (engine === 'rustfs') return 'rustfs'
  if (['postgres', 'redis', 'mongodb', 's3'].includes(engine)) return engine as EngineKind
  return 'postgres'
}

function formatMetricValue(name: string, value: number): string {
  if (name.endsWith('_bytes') || name.endsWith('_bytes_total')) return formatBytes(value)
  if (name.endsWith('_ratio')) return `${(value * 100).toFixed(1)}%`
  if (name.endsWith('_percent') || name.endsWith('_usage')) return `${value.toFixed(1)}%`
  if (name.endsWith('_seconds') || name.endsWith('_sec')) return `${value.toFixed(2)}s`
  if (name.endsWith('_ms')) return `${value.toFixed(0)}ms`
  // Counters and totals are event counts — always display as integers
  if (name.endsWith('_total') || name.endsWith('.total') || name.endsWith('_count') || name.endsWith('.count'))
    return Math.round(value).toString()
  if (Number.isInteger(value)) return value.toString()
  return value.toFixed(2)
}

const METRIC_LABELS: Record<string, string> = {
  'rustfs_cluster_buckets_total': 'Buckets',
  'rustfs_cluster_objects_total': 'Objects',
  'rustfs_cluster_capacity_usable_total_bytes': 'Usable Capacity',
  'rustfs_cluster_capacity_used_bytes': 'Used Capacity',
  'rustfs_cluster_capacity_free_bytes': 'Free Capacity',
  'rustfs_cluster_capacity_raw_total_bytes': 'Raw Capacity',
  'rustfs_node_disk_total_bytes': 'Disk Total',
  'rustfs_node_disk_used_bytes': 'Disk Used',
  'rustfs_node_disk_free_bytes': 'Disk Free',
  'rustfs_process_cpu_percent': 'CPU %',
  'rustfs_process_memory_bytes': 'Memory',
  'rustfs_process_uptime_seconds': 'Uptime',
  'rustfs_s3_operations_total': 'S3 Operations',
  'rustfs.api.requests.total': 'API Requests',
  'rustfs.request.body.bytes_total': 'Request Bytes',
  'rustfs_system_process_cpu_usage': 'CPU Usage',
  'rustfs_system_process_resident_memory_bytes': 'Resident Memory',
}

function labelForMetric(name: string): string {
  if (METRIC_LABELS[name]) return METRIC_LABELS[name]
  const bare = name.replace(/^[a-z0-9]+[._]/, '')
  return bare.replace(/[._]/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase())
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RANGE_OPTIONS = [
  { label: '1h', value: '1h' },
  { label: '6h', value: '6h' },
  { label: '24h', value: '24h' },
  { label: '7d', value: '7d' },
]

const CHART_LINE_COLOR = '#2563eb'

// ---------------------------------------------------------------------------
// MetricTile — a single clickable stat item rendered inside a <dl> grid
// ---------------------------------------------------------------------------

type MetricTileProps = {
  name: string
  value: number | undefined
  selected: boolean
  onClick: () => void
  alert?: 'warning' | 'critical'
  size?: 'hero' | 'group'
}

function MetricTile({ name, value, selected, onClick, alert, size = 'group' }: MetricTileProps) {
  const isHero = size === 'hero'
  return (
    <button
      type="button"
      onClick={onClick}
      className={[
        'flex flex-col text-left transition-colors',
        isHero ? 'gap-1 px-4 py-4' : 'gap-0.5 px-4 py-3',
        'hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary',
        selected ? 'bg-primary/5 ring-1 ring-inset ring-primary/30' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <dt
        className={[
          'flex items-center gap-1.5 truncate font-medium text-muted-foreground',
          isHero ? 'text-[11px]' : 'text-[11px]',
        ].join(' ')}
      >
        {alert === 'critical' && (
          <span className="inline-block size-1.5 shrink-0 rounded-full bg-destructive" />
        )}
        {alert === 'warning' && (
          <span className="inline-block size-1.5 shrink-0 rounded-full bg-amber-400" />
        )}
        <span className="truncate">{labelForMetric(name)}</span>
      </dt>
      <dd
        className={[
          'font-semibold tabular-nums text-foreground',
          isHero ? 'text-2xl' : 'text-lg',
        ].join(' ')}
      >
        {value != null ? formatMetricValue(name, value) : '—'}
      </dd>
    </button>
  )
}

// ---------------------------------------------------------------------------
// MetricChart — single time-series line chart
// ---------------------------------------------------------------------------

type MetricChartProps = {
  serviceId: number
  metricName: string
  range: string
}

function MetricChart({ serviceId, metricName, range }: MetricChartProps) {
  const { data, isLoading } = useQuery<MetricRangePoint[]>({
    queryKey: ['svc-monitoring-range', serviceId, metricName, range],
    queryFn: () => fetchMetricRange(serviceId, metricName, range),
    staleTime: 15_000,
    refetchInterval: 30_000,
  })

  const chartData = (data ?? []).map((p) => ({
    time: new Date(p.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
    value: p.value,
  }))

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin text-muted-foreground" />
        Loading…
      </div>
    )
  }

  if (chartData.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        No data for this range
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height="100%">
      <LineChart data={chartData} margin={{ top: 4, right: 8, left: 0, bottom: 0 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="rgba(128,128,128,0.15)" vertical={false} />
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
            metricName.endsWith('_ratio') || metricName.endsWith('_percent')
              ? 52
              : metricName.endsWith('_bytes') || metricName.endsWith('_bytes_total')
                ? 70
                : 44
          }
          tickFormatter={(v: number) => formatMetricValue(metricName, v)}
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
          labelStyle={{ color: 'hsl(var(--muted-foreground))', fontSize: 11, marginBottom: 2 }}
          itemStyle={{ color: CHART_LINE_COLOR }}
          cursor={{ stroke: 'rgba(128,128,128,0.3)', strokeWidth: 1 }}
          formatter={(v: number) => [formatMetricValue(metricName, v), labelForMetric(metricName)]}
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
  )
}

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
  const [metricName, setMetricName] = useState(ALL_METRICS[engine][0] ?? '')
  const [threshold, setThreshold] = useState('0')
  const [comparator, setComparator] =
    useState<ServiceAlertRuleCreateRequest['comparator']>('gt')
  const [severity, setSeverity] =
    useState<ServiceAlertRuleCreateRequest['severity']>('warning')

  const create = useMutation({
    mutationFn: () =>
      createAlertRule(serviceId, {
        name,
        metric_name: metricName,
        comparator,
        threshold: parseFloat(threshold),
        severity,
      }),
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
            <label className="text-sm font-medium text-foreground">Rule name</label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. High connection count"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-foreground">Metric</label>
            <Select value={metricName} onValueChange={setMetricName}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {ALL_METRICS[engine].map((m) => (
                  <SelectItem key={m} value={m}>
                    {labelForMetric(m)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex gap-3">
            <div className="w-32 space-y-1.5">
              <label className="text-sm font-medium text-foreground">Comparator</label>
              <Select
                value={comparator}
                onValueChange={(v) =>
                  setComparator(v as ServiceAlertRuleCreateRequest['comparator'])
                }
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
              <label className="text-sm font-medium text-foreground">Threshold</label>
              <Input
                type="number"
                value={threshold}
                onChange={(e) => setThreshold(e.target.value)}
              />
            </div>
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-foreground">Severity</label>
            <Select
              value={severity}
              onValueChange={(v) =>
                setSeverity(v as ServiceAlertRuleCreateRequest['severity'])
              }
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
          <Button onClick={() => create.mutate()} disabled={create.isPending || !name.trim()}>
            {create.isPending && <Loader2 className="mr-2 size-4 animate-spin" />}
            Add Rule
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// AlertRulesSection
// ---------------------------------------------------------------------------

type AlertRulesSectionProps = {
  serviceId: number
  engine: EngineKind
}

function AlertRulesSection({ serviceId, engine }: AlertRulesSectionProps) {
  const queryClient = useQueryClient()
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [ruleToDelete, setRuleToDelete] = useState<ServiceAlertRule | null>(null)

  const { data: rules, isLoading } = useQuery<ServiceAlertRule[]>({
    queryKey: ['svc-monitoring-alert-rules', serviceId],
    queryFn: () => fetchAlertRules(serviceId),
    staleTime: 30_000,
    refetchInterval: 30_000,
  })

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ['svc-monitoring-alert-rules', serviceId] })

  const removeRule = useMutation({
    mutationFn: (ruleId: number) => deleteAlertRule(serviceId, ruleId),
    onSuccess: () => {
      toast.success('Alert rule deleted')
      setRuleToDelete(null)
      invalidate()
    },
    onError: (err: Error) => toast.error('Failed to delete rule', { description: err.message }),
  })

  const firingCount = rules?.filter((r) => r.status === 'firing').length ?? 0

  return (
    <>
      <div>
        <div className="mb-3 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <h2 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
              Alert Rules
            </h2>
            {firingCount > 0 && (
              <Badge variant="destructive" className="text-xs">
                {firingCount} firing
              </Badge>
            )}
          </div>
          <Button
            variant="outline"
            size="sm"
            className="gap-1.5"
            onClick={() => setAddDialogOpen(true)}
          >
            <Plus className="size-3.5" />
            Add Rule
          </Button>
        </div>

        {isLoading ? (
          <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
            Loading rules…
          </div>
        ) : !rules || rules.length === 0 ? (
          <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-10 text-center">
            <Activity className="size-6 text-muted-foreground" />
            <p className="max-w-xs text-sm text-muted-foreground">
              No alert rules yet. Add one to get notified when metrics cross a threshold.
            </p>
          </div>
        ) : (
          <div className="overflow-x-auto rounded-lg border border-border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead className="hidden sm:table-cell">Metric</TableHead>
                  <TableHead>Condition</TableHead>
                  <TableHead className="hidden sm:table-cell">Severity</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="w-8" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {rules.map((rule) => (
                  <TableRow key={rule.id} className="even:bg-muted/30">
                    <TableCell className="font-medium text-foreground">{rule.name}</TableCell>
                    <TableCell className="hidden font-mono text-xs text-muted-foreground sm:table-cell">
                      {rule.metric_name}
                    </TableCell>
                    <TableCell className="text-sm tabular-nums text-foreground">
                      {rule.comparator} {formatMetricValue(rule.metric_name, rule.threshold)}
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
                        className="text-xs capitalize"
                      >
                        {rule.severity}
                      </Badge>
                    </TableCell>
                    <TableCell>
                      <Badge
                        variant={rule.status === 'firing' ? 'destructive' : 'outline'}
                        className="text-xs"
                      >
                        {rule.status === 'firing' ? 'FIRING' : 'OK'}
                      </Badge>
                    </TableCell>
                    <TableCell>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-7 text-muted-foreground hover:text-destructive"
                        onClick={() => setRuleToDelete(rule)}
                      >
                        <Trash2 className="size-3.5" />
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </div>

      <AddAlertRuleDialog
        open={addDialogOpen}
        onOpenChange={setAddDialogOpen}
        serviceId={serviceId}
        engine={engine}
        onSuccess={invalidate}
      />

      <Dialog open={!!ruleToDelete} onOpenChange={(o) => { if (!o) setRuleToDelete(null) }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Alert Rule</DialogTitle>
            <DialogDescription>
              Delete{' '}
              <span className="font-medium text-foreground">{ruleToDelete?.name}</span>? This
              cannot be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setRuleToDelete(null)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={removeRule.isPending}
              onClick={() => ruleToDelete && removeRule.mutate(ruleToDelete.id)}
            >
              {removeRule.isPending && <Loader2 className="mr-2 size-4 animate-spin" />}
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}

// ---------------------------------------------------------------------------
// MonitoringDashboard — the main content once data is available
// ---------------------------------------------------------------------------

type MonitoringDashboardProps = {
  serviceId: number
  engine: EngineKind
  latestMetrics: MetricLatest[]
}

function MonitoringDashboard({
  serviceId,
  engine,
  latestMetrics,
}: MonitoringDashboardProps) {
  const groups = ENGINE_GROUPS[engine]
  const heroMetrics = HERO_METRICS[engine]

  const [selectedMetric, setSelectedMetric] = useState(
    heroMetrics[0] ?? groups[0]?.metrics[0] ?? '',
  )
  const [range, setRange] = useState('1h')

  const latestByName = new Map<string, number>()
  for (const m of latestMetrics) {
    latestByName.set(m.name, m.value)
  }

  // Build a set of metrics that are firing alerts for visual callouts
  const { data: alertRules } = useQuery<ServiceAlertRule[]>({
    queryKey: ['svc-monitoring-alert-rules', serviceId],
    queryFn: () => fetchAlertRules(serviceId),
    staleTime: 30_000,
    refetchInterval: 30_000,
  })

  const firingBySeverity = new Map<string, 'warning' | 'critical'>()
  for (const rule of alertRules ?? []) {
    if (rule.status === 'firing') {
      // critical takes precedence over warning
      const existing = firingBySeverity.get(rule.metric_name)
      if (!existing || rule.severity === 'critical') {
        firingBySeverity.set(rule.metric_name, rule.severity as 'warning' | 'critical')
      }
    }
  }

  const firingCount = (alertRules ?? []).filter((r) => r.status === 'firing').length

  return (
    <div className="space-y-8">
      {/* Firing alert banner */}
      {firingCount > 0 && (
        <div className="flex items-center gap-2 rounded-md bg-destructive/10 px-3 py-2.5 text-sm text-destructive ring-1 ring-inset ring-destructive/20">
          <span className="inline-block size-1.5 shrink-0 rounded-full bg-destructive" />
          <span className="font-medium">
            {firingCount} alert{firingCount > 1 ? 's' : ''} firing
          </span>
          <Link
            to="/monitoring/alarms"
            className="ml-auto text-xs underline underline-offset-2 hover:no-underline"
          >
            View all
          </Link>
        </div>
      )}

      {/* Hero stats */}
      <div>
        <div className="mb-2">
          <h2 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
            Overview
          </h2>
        </div>
        <dl className="grid grid-cols-2 divide-x divide-y divide-border rounded-lg border border-border sm:grid-cols-4 sm:divide-y-0">
          {heroMetrics.map((name) => (
            <MetricTile
              key={name}
              name={name}
              value={latestByName.get(name)}
              selected={selectedMetric === name}
              onClick={() => setSelectedMetric(name)}
              alert={firingBySeverity.get(name)}
              size="hero"
            />
          ))}
        </dl>
      </div>

      {/* Chart panel */}
      <div className="rounded-lg border border-border bg-card">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border px-4 py-3">
          <div className="min-w-0">
            <p className="truncate text-sm font-medium text-foreground">
              {labelForMetric(selectedMetric)}
            </p>
            <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">{selectedMetric}</p>
          </div>
          <div className="flex shrink-0 gap-1">
            {RANGE_OPTIONS.map((opt) => (
              <Button
                key={opt.value}
                variant={range === opt.value ? 'default' : 'outline'}
                size="sm"
                className="h-7 px-2.5 text-xs"
                onClick={() => setRange(opt.value)}
              >
                {opt.label}
              </Button>
            ))}
          </div>
        </div>
        <div className="h-56 px-2 py-4 overflow-visible">
          <MetricChart serviceId={serviceId} metricName={selectedMetric} range={range} />
        </div>
      </div>

      {/* Grouped metric sections */}
      {groups.map((group) => (
        <div key={group.title}>
          <div className="mb-2">
            <h2 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
              {group.title}
            </h2>
          </div>
          <dl className="grid grid-cols-2 divide-x divide-y divide-border rounded-lg border border-border sm:grid-cols-3 lg:grid-cols-4">
            {group.metrics.map((name) => (
              <MetricTile
                key={name}
                name={name}
                value={latestByName.get(name)}
                selected={selectedMetric === name}
                onClick={() => setSelectedMetric(name)}
                alert={firingBySeverity.get(name)}
              />
            ))}
          </dl>
        </div>
      ))}

      {/* Alert rules */}
      <AlertRulesSection serviceId={serviceId} engine={engine} />
    </div>
  )
}

// ---------------------------------------------------------------------------
// ServiceMonitoring — page root
// ---------------------------------------------------------------------------

export function ServiceMonitoring() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const serviceId = id ? parseInt(id) : 0

  const { data: serviceData, isLoading: serviceLoading } = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: !!serviceId,
  })

  const engine = normalizeEngine(
    serviceData?.service?.service_type ?? '',
    serviceData?.current_parameters?.docker_image,
  )

  const {
    data: latestMetrics,
    isLoading: metricsLoading,
    error: metricsError,
    refetch,
    isFetching,
  } = useQuery<MetricLatest[], Error>({
    queryKey: ['svc-monitoring-latest', serviceId],
    queryFn: () => fetchLatestMetrics(serviceId),
    enabled: !!serviceId,
    staleTime: 15_000,
    refetchInterval: 30_000,
    retry: (failureCount, err) => {
      const msg = err.message.toLowerCase()
      if (
        msg.includes('not enabled') ||
        msg.includes('not found') ||
        msg.includes('http 404') ||
        msg.includes('http 503')
      )
        return false
      return failureCount < 2
    },
  })

  const isDisabled =
    metricsError != null &&
    (metricsError.message.toLowerCase().includes('not enabled') ||
      metricsError.message.toLowerCase().includes('not found') ||
      metricsError.message.includes('HTTP 404') ||
      metricsError.message.includes('HTTP 503'))

  const serviceName = serviceData?.service?.name ?? 'Service'

  const handleRefresh = () => {
    refetch()
    queryClient.invalidateQueries({ queryKey: ['svc-monitoring-alert-rules', serviceId] })
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="p-4 space-y-6 md:p-6">
      {/* Page header */}
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          {/* Breadcrumb */}
          <div className="flex min-w-0 items-center gap-1.5 mb-1">
            <button
              type="button"
              onClick={() => navigate(`/storage/${id}`)}
              className="text-muted-foreground hover:text-foreground transition-colors"
            >
              <ArrowLeft className="size-3.5" />
            </button>
            <span className="text-muted-foreground text-xs">/</span>
            <button
              type="button"
              onClick={() => navigate(`/storage/${id}`)}
              className="truncate text-xs text-muted-foreground hover:text-foreground transition-colors"
            >
              {serviceName}
            </button>
            <span className="text-muted-foreground text-xs">/</span>
            <span className="text-xs text-muted-foreground">Monitoring</span>
          </div>
          {/* Page title */}
          <h1 className="text-xl font-semibold text-foreground truncate">
            {serviceName}
          </h1>
          <p className="text-sm text-muted-foreground mt-0.5">
            Real-time metrics and performance monitoring
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          className="gap-1.5 shrink-0"
          onClick={handleRefresh}
          disabled={isFetching}
        >
          <RefreshCw className={`size-3.5 ${isFetching ? 'animate-spin' : ''}`} />
          <span>Refresh</span>
        </Button>
      </div>

      {/* Body */}
      {serviceLoading || metricsLoading ? (
        <div className="flex flex-col items-center justify-center gap-2 py-16 text-center">
          <Loader2 className="size-5 animate-spin text-muted-foreground" />
          <p className="text-sm text-muted-foreground">Loading metrics…</p>
        </div>
      ) : isDisabled ? (
        <div className="flex flex-col items-center gap-4 rounded-lg border border-dashed border-border bg-card p-12 text-center">
          <Activity className="size-8 text-muted-foreground" />
          <div>
            <p className="text-sm font-medium text-foreground">Monitoring not enabled</p>
            <p className="mt-1 text-sm text-muted-foreground">
              Enable monitoring on the service page to start collecting metrics.
            </p>
          </div>
          <Button variant="outline" onClick={() => navigate(`/storage/${id}`)}>
            Go to service
          </Button>
        </div>
      ) : !latestMetrics || latestMetrics.length === 0 ? (
        <div className="flex items-center gap-3 rounded-lg border border-border bg-card p-6">
          <span className="relative flex size-3">
            <span className="absolute inline-flex size-full animate-ping rounded-full bg-primary opacity-75" />
            <span className="relative inline-flex size-3 rounded-full bg-primary" />
          </span>
          <div>
            <p className="text-sm font-medium text-foreground">Collecting first metrics…</p>
            <p className="text-sm text-muted-foreground">First metrics appear within 30 seconds.</p>
          </div>
        </div>
      ) : (
        <MonitoringDashboard
          serviceId={serviceId}
          engine={engine}
          latestMetrics={latestMetrics}
        />
      )}
      </div>
    </div>
  )
}
