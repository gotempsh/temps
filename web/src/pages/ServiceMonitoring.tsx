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
import { TOOLTIP_CONTENT_STYLE, TOOLTIP_LABEL_STYLE } from '@/lib/chart-tooltip'
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
import {
  getServiceOptions,
  externalServiceMetricsStatusOptions,
  externalServiceMetricsByDatabaseOptions,
  externalServiceMetricsGetLatestOptions,
  externalServiceMetricsGetRangeOptions,
  externalServiceMetricsGetAlertRulesOptions,
  externalServiceMetricsGetAlertRulesQueryKey,
  externalServiceMetricsCreateAlertRuleMutation,
  externalServiceMetricsDeleteAlertRuleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { ServiceAlertRuleResponse } from '@/api/client/types.gen'
import {
  Activity,
  ArrowLeft,
  Loader2,
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { createContext, useContext, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { usePageTitle } from '@/hooks/usePageTitle'
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
// View-model types (derived from the generated SDK responses)
// ---------------------------------------------------------------------------

/** A single metric reading — normalised from the `{ name: value }` map the
 *  `metrics/latest` endpoint returns. */
type MetricLatest = { name: string; value: number }

/** Alert-rule form-state unions. The API accepts `comparator`/`severity` as
 *  plain strings; these constrain the UI selects to the supported values. */
type Comparator = 'gt' | 'lt' | 'gte' | 'lte'
type Severity = 'info' | 'warning' | 'critical'

/** Extract a comparable message from whatever the SDK throws on a failed
 *  request. `@hey-api/client-fetch` throws the parsed RFC 7807 Problem body
 *  ({ detail, title, status }). */
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
// Metric metadata per engine
// ---------------------------------------------------------------------------

type EngineKind = 'postgres' | 'redis' | 'mongodb' | 's3' | 'rustfs'

type MetricGroup = {
  title: string
  metrics: string[]
}

// NOTE (Postgres scope): metrics split into two buckets.
//   * GLOBAL (here, in ENGINE_GROUPS): instance-wide stats that have no
//     per-database breakdown — connection/lock state (`pg_stat_activity` is
//     cluster-wide), table-level tuple/scan stats (aggregated across all user
//     tables), and WAL/replication (single cluster-wide stream).
//   * PER-DATABASE (PG_PER_DATABASE_GROUPS): emitted once per `datname` plus an
//     instance aggregate. Shown in the dedicated "Databases" section with a
//     database selector, NOT in these top tiles, so a per-db sum never sits
//     unlabelled next to a true global metric.
const ENGINE_GROUPS: Record<EngineKind, MetricGroup[]> = {
  postgres: [
    {
      title: 'Connections',
      metrics: [
        'pg.connections',
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
      title: 'Tables',
      metrics: [
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

// Per-database Postgres metric groups, shown in the dedicated "Databases"
// section with a database selector. Each metric is emitted once per `datname`
// plus an instance-wide aggregate (selector value "All databases"). Order /
// titles mirror the old Performance + Throughput tile groups.
const PG_PER_DATABASE_GROUPS: MetricGroup[] = [
  {
    title: 'Storage',
    metrics: ['pg.database_size_bytes'],
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
    title: 'Write Activity',
    metrics: [
      'pg.tuples_inserted_total',
      'pg.tuples_updated_total',
      'pg.tuples_deleted_total',
    ],
  },
]

// Flat list of every per-database metric name (for the by-database query).
const PG_PER_DATABASE_METRICS: string[] = PG_PER_DATABASE_GROUPS.flatMap(
  (g) => g.metrics
)

// All known metrics for alert rule creation. For Postgres this includes both
// the global metrics (ENGINE_GROUPS) and the per-database metrics so alert
// rules can target either scope.
const ALL_METRICS: Record<EngineKind, string[]> = Object.fromEntries(
  Object.entries(ENGINE_GROUPS).map(([engine, groups]) => [
    engine,
    [
      ...groups.flatMap((g) => g.metrics),
      ...(engine === 'postgres' ? PG_PER_DATABASE_METRICS : []),
    ],
  ])
) as Record<EngineKind, string[]>

// Headline metrics shown in the hero row
const HERO_METRICS: Record<EngineKind, string[]> = {
  postgres: [
    'pg.connections',
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
  s3: [
    's3.bucket_count',
    's3.total_size_bytes',
    's3.capacity_usable_total_bytes',
    's3.object_count',
  ],
  rustfs: [
    'rustfs_cluster_buckets_total',
    'rustfs_cluster_capacity_used_bytes',
    'rustfs_cluster_capacity_free_bytes',
    'rustfs_s3_operations_total',
  ],
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

function normalizeEngine(
  engine: string,
  dockerImage?: string | null
): EngineKind {
  // An s3 service running the rustfs image should use rustfs metric groups
  if (engine === 's3' && dockerImage?.toLowerCase().includes('rustfs'))
    return 'rustfs'
  if (engine === 'rustfs') return 'rustfs'
  if (['postgres', 'redis', 'mongodb', 's3'].includes(engine))
    return engine as EngineKind
  return 'postgres'
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
  if (name.endsWith('_bytes') || name.endsWith('_bytes_total'))
    return formatBytes(value)
  if (name.endsWith('_ratio')) return `${(value * 100).toFixed(1)}%`
  if (name.endsWith('_percent') || name.endsWith('_usage'))
    return `${value.toFixed(1)}%`
  if (name.endsWith('_seconds') || name.endsWith('_sec'))
    return `${value.toFixed(2)}s`
  if (name.endsWith('_ms')) return `${value.toFixed(0)}ms`
  // Counters and totals are event counts — always display as integers
  if (
    name.endsWith('_total') ||
    name.endsWith('.total') ||
    name.endsWith('_count') ||
    name.endsWith('.count')
  )
    return Math.round(value).toString()
  if (Number.isInteger(value)) return value.toString()
  return value.toFixed(2)
}

const METRIC_LABELS: Record<string, string> = {
  // Postgres connections — "Total" is the headline (client backends only;
  // engine background processes are excluded by the collector).
  'pg.connections': 'Connections',
  'pg.connections_active': 'Active',
  'pg.connections_idle': 'Idle',
  'pg.connections_idle_in_transaction': 'Idle in Transaction',
  'pg.connections_other': 'Other',
  rustfs_cluster_buckets_total: 'Buckets',
  rustfs_cluster_objects_total: 'Objects',
  rustfs_cluster_capacity_usable_total_bytes: 'Usable Capacity',
  rustfs_cluster_capacity_used_bytes: 'Used Capacity',
  rustfs_cluster_capacity_free_bytes: 'Free Capacity',
  rustfs_cluster_capacity_raw_total_bytes: 'Raw Capacity',
  rustfs_node_disk_total_bytes: 'Disk Total',
  rustfs_node_disk_used_bytes: 'Disk Used',
  rustfs_node_disk_free_bytes: 'Disk Free',
  rustfs_process_cpu_percent: 'CPU %',
  rustfs_process_memory_bytes: 'Memory',
  rustfs_process_uptime_seconds: 'Uptime',
  rustfs_s3_operations_total: 'S3 Operations',
  'rustfs.api.requests.total': 'API Requests',
  'rustfs.request.body.bytes_total': 'Request Bytes',
  rustfs_system_process_cpu_usage: 'CPU Usage',
  rustfs_system_process_resident_memory_bytes: 'Resident Memory',
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
// Auto-refresh interval — shared across all monitoring queries on the page.
// `null` means auto-refresh is off (manual Refresh only).
// ---------------------------------------------------------------------------

const REFRESH_OPTIONS: { label: string; value: number | null }[] = [
  { label: 'Off', value: null },
  { label: '5s', value: 5_000 },
  { label: '10s', value: 10_000 },
  { label: '30s', value: 30_000 },
  { label: '1m', value: 60_000 },
]

const REFRESH_STORAGE_KEY = 'temps:monitoring:refresh-interval'

/** Refetch interval in ms, or `false` to disable (React Query's convention). */
const RefreshIntervalContext = createContext<number | false>(30_000)

/** Read the active refetch interval for monitoring queries. */
function useRefreshInterval(): number | false {
  return useContext(RefreshIntervalContext)
}

function loadStoredRefreshInterval(): number | null {
  try {
    const raw = localStorage.getItem(REFRESH_STORAGE_KEY)
    if (raw === null) return 30_000
    if (raw === 'off') return null
    const n = parseInt(raw, 10)
    return Number.isFinite(n) ? n : 30_000
  } catch {
    return 30_000
  }
}

function storeRefreshInterval(value: number | null): void {
  try {
    localStorage.setItem(
      REFRESH_STORAGE_KEY,
      value === null ? 'off' : String(value)
    )
  } catch {
    // ignore — non-critical
  }
}

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

function MetricTile({
  name,
  value,
  selected,
  onClick,
  alert,
  size = 'group',
}: MetricTileProps) {
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
  const refetchInterval = useRefreshInterval()
  const { data, isLoading } = useQuery({
    ...externalServiceMetricsGetRangeOptions({
      path: { id: serviceId },
      query: { metric: metricName, range },
    }),
    staleTime: 15_000,
    refetchInterval,
  })

  const chartData = (data ?? []).map((p) => ({
    time: new Date(p.time).toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
    }),
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
      {/* Right margin leaves room so the last data point isn't flush against
          the panel edge — otherwise its hover tooltip renders past the edge and
          gets clipped by the scroll container. */}
      <LineChart
        data={chartData}
        margin={{ top: 4, right: 24, left: 0, bottom: 0 }}
      >
        <CartesianGrid
          strokeDasharray="3 3"
          stroke="rgba(128,128,128,0.15)"
          vertical={false}
        />
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
              : metricName.endsWith('_bytes') ||
                  metricName.endsWith('_bytes_total')
                ? 70
                : 44
          }
          tickFormatter={(v: number) => formatMetricValue(metricName, v)}
        />
        <Tooltip
          wrapperStyle={{ zIndex: 50 }}
          allowEscapeViewBox={{ x: true, y: true }}
          contentStyle={TOOLTIP_CONTENT_STYLE}
          labelStyle={TOOLTIP_LABEL_STYLE}
          itemStyle={{ color: CHART_LINE_COLOR }}
          cursor={{ stroke: 'rgba(128,128,128,0.3)', strokeWidth: 1 }}
          formatter={(v: number) => [
            formatMetricValue(metricName, v),
            labelForMetric(metricName),
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
            <label className="text-sm font-medium text-foreground">
              Rule name
            </label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. High connection count"
            />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-foreground">
              Metric
            </label>
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
              <label className="text-sm font-medium text-foreground">
                Comparator
              </label>
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
              <label className="text-sm font-medium text-foreground">
                Threshold
              </label>
              <Input
                type="number"
                value={threshold}
                onChange={(e) => setThreshold(e.target.value)}
              />
            </div>
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium text-foreground">
              Severity
            </label>
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
              <Loader2 className="mr-2 size-4 animate-spin" />
            )}
            Add Rule
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ---------------------------------------------------------------------------
// DatabasesSection — per-database breakdown for Postgres services
// ---------------------------------------------------------------------------

// Sentinel for the "All databases" selector option (instance-wide aggregate).
const ALL_DATABASES = '__all__'

type DatabasesSectionProps = {
  serviceId: number
  /** Instance-wide aggregate values (from query_latest) for "All databases". */
  aggregateByName: Map<string, number>
}

/**
 * Per-database Postgres metrics with a database selector.
 *
 * A Postgres instance can host many unrelated databases; these metrics
 * (`pg_stat_database` + size) are emitted once per `datname`. The selector
 * chooses "All databases" (the instance aggregate) or a single database, and
 * the tile grids re-render for that scope. Kept separate from the global tiles
 * so a per-db sum is never shown unlabelled next to a true instance metric.
 */
function DatabasesSection({
  serviceId,
  aggregateByName,
}: DatabasesSectionProps) {
  const refetchInterval = useRefreshInterval()
  const [selected, setSelected] = useState<string>(ALL_DATABASES)

  const { data, isLoading } = useQuery({
    ...externalServiceMetricsByDatabaseOptions({ path: { id: serviceId } }),
    refetchInterval,
  })

  const databases = data?.databases ?? []

  // Don't render until at least one database has reported per-db metrics.
  if (!isLoading && databases.length === 0) return null

  // Resolve the value map for the current selection.
  const valuesForSelection = (): Map<string, number> => {
    if (selected === ALL_DATABASES) return aggregateByName
    const db = databases.find((d) => d.database === selected)
    const m = new Map<string, number>()
    if (db?.metrics) {
      for (const [name, value] of Object.entries(db.metrics)) m.set(name, value)
    }
    return m
  }
  const values = valuesForSelection()

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h2 className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
          Databases
        </h2>
        <Select value={selected} onValueChange={setSelected}>
          <SelectTrigger className="w-full sm:w-[260px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL_DATABASES}>All databases</SelectItem>
            {databases.map((db) => (
              <SelectItem key={db.database} value={db.database}>
                {db.database}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {PG_PER_DATABASE_GROUPS.map((group) => (
        <div key={group.title}>
          <div className="mb-2">
            <h3 className="text-[10px] font-medium uppercase tracking-widest text-muted-foreground/70">
              {group.title}
            </h3>
          </div>
          <dl className="grid grid-cols-2 divide-x divide-y divide-border rounded-lg border border-border sm:grid-cols-3 lg:grid-cols-4">
            {group.metrics.map((name) => (
              <MetricTile
                key={name}
                name={name}
                value={values.get(name)}
                selected={false}
                onClick={() => {}}
              />
            ))}
          </dl>
        </div>
      ))}
    </div>
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
  const refetchInterval = useRefreshInterval()
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [ruleToDelete, setRuleToDelete] =
    useState<ServiceAlertRuleResponse | null>(null)

  const { data: rules, isLoading } = useQuery({
    ...externalServiceMetricsGetAlertRulesOptions({ path: { id: serviceId } }),
    staleTime: 30_000,
    refetchInterval,
  })

  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: externalServiceMetricsGetAlertRulesQueryKey({
        path: { id: serviceId },
      }),
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

  // The backend does not surface a per-rule firing state, so the badge count
  // stays at zero (same as before — `status` was never populated).
  const firingCount = 0

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
              No alert rules yet. Add one to get notified when metrics cross a
              threshold.
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
                  <TableHead className="hidden sm:table-cell">
                    Severity
                  </TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="w-8" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {rules.map((rule) => (
                  <TableRow key={rule.id} className="even:bg-muted/30">
                    <TableCell className="font-medium text-foreground">
                      {rule.name}
                    </TableCell>
                    <TableCell className="hidden font-mono text-xs text-muted-foreground sm:table-cell">
                      {rule.metric_name}
                    </TableCell>
                    <TableCell className="text-sm tabular-nums text-foreground">
                      {rule.comparator}{' '}
                      {formatMetricValue(rule.metric_name, rule.threshold)}
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
                      <Badge variant="outline" className="text-xs">
                        OK
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
              Delete{' '}
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
                <Loader2 className="mr-2 size-4 animate-spin" />
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
  const refetchInterval = useRefreshInterval()
  const groups = ENGINE_GROUPS[engine]
  const heroMetrics = HERO_METRICS[engine]

  const [selectedMetric, setSelectedMetric] = useState(
    heroMetrics[0] ?? groups[0]?.metrics[0] ?? ''
  )
  const [range, setRange] = useState('1h')

  const latestByName = new Map<string, number>()
  for (const m of latestMetrics) {
    latestByName.set(m.name, m.value)
  }

  // Keep the alert rules warm so the section opens instantly. The backend does
  // not surface a per-rule firing state, so no tile/banner callouts fire (same
  // behaviour as before — `status` was never populated).
  useQuery({
    ...externalServiceMetricsGetAlertRulesOptions({ path: { id: serviceId } }),
    staleTime: 30_000,
    refetchInterval,
  })

  const firingBySeverity = new Map<string, 'warning' | 'critical'>()

  const firingCount = 0

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
            <p className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
              {selectedMetric}
            </p>
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
          <MetricChart
            serviceId={serviceId}
            metricName={selectedMetric}
            range={range}
          />
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

      {/* Per-database breakdown (Postgres) */}
      {engine === 'postgres' && (
        <DatabasesSection
          serviceId={serviceId}
          aggregateByName={latestByName}
        />
      )}

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

  // Auto-refresh interval (ms) or null = off. Persisted in localStorage so the
  // user's choice survives navigation/reload.
  const [refreshMs, setRefreshMs] = useState<number | null>(
    loadStoredRefreshInterval
  )
  const refetchInterval: number | false = refreshMs ?? false

  const serviceId = id ? parseInt(id) : 0

  const { data: serviceData, isLoading: serviceLoading } = useQuery({
    ...getServiceOptions({ path: { id: serviceId } }),
    enabled: !!serviceId,
  })

  const engine = normalizeEngine(
    serviceData?.service?.service_type ?? '',
    serviceData?.current_parameters?.docker_image
  )

  usePageTitle(
    serviceData?.service?.name
      ? `${serviceData.service.name} · Monitoring`
      : 'Monitoring'
  )

  const {
    data: latestMetrics,
    isLoading: metricsLoading,
    error: metricsError,
    refetch,
    isFetching,
  } = useQuery({
    ...externalServiceMetricsGetLatestOptions({ path: { id: serviceId } }),
    // Normalise the `{ name: value }` map into the array shape the dashboard
    // consumes.
    select: (map): MetricLatest[] =>
      Object.entries(map).map(([name, value]) => ({ name, value })),
    enabled: !!serviceId,
    staleTime: 15_000,
    refetchInterval: (query) => {
      // Stop polling on permanent errors (monitoring disabled on the server or
      // this service, service not found) — otherwise we poll forever.
      if (query.state.error && isMonitoringUnavailable(query.state.error)) {
        return false
      }
      return refetchInterval
    },
    retry: (failureCount, err) => {
      if (isMonitoringUnavailable(err)) return false
      return failureCount < 2
    },
  })

  const isDisabled =
    metricsError != null && isMonitoringUnavailable(metricsError)

  const serviceName = serviceData?.service?.name ?? 'Service'

  // Freshness status — cheap O(1) lookup of when metrics were last received.
  const { data: statusData } = useQuery({
    ...externalServiceMetricsStatusOptions({ path: { id: serviceId } }),
    enabled: !!serviceId && !isDisabled,
    refetchInterval,
  })
  const lastReceivedAt = statusData?.last_received_at ?? null

  const handleRefresh = () => {
    refetch()
    queryClient.invalidateQueries({
      queryKey: externalServiceMetricsGetAlertRulesQueryKey({
        path: { id: serviceId },
      }),
    })
  }

  return (
    <RefreshIntervalContext.Provider value={refetchInterval}>
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
                <span className="text-xs text-muted-foreground">
                  Monitoring
                </span>
              </div>
              {/* Page title */}
              <h1 className="text-xl font-semibold text-foreground truncate">
                {serviceName}
              </h1>
              <p className="text-sm text-muted-foreground mt-0.5">
                Real-time metrics and performance monitoring
                {lastReceivedAt && (
                  <span>
                    {' '}
                    · last received {formatRelativeTime(lastReceivedAt)}
                  </span>
                )}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              {/* Auto-refresh interval */}
              <Select
                value={refreshMs === null ? 'off' : String(refreshMs)}
                onValueChange={(v) => {
                  const next = v === 'off' ? null : parseInt(v, 10)
                  setRefreshMs(next)
                  storeRefreshInterval(next)
                }}
              >
                <SelectTrigger className="h-8 w-[88px] gap-1.5 text-xs">
                  <RefreshCw className="size-3.5 shrink-0 text-muted-foreground" />
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {REFRESH_OPTIONS.map((opt) => (
                    <SelectItem
                      key={opt.label}
                      value={opt.value === null ? 'off' : String(opt.value)}
                    >
                      {opt.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Button
                variant="outline"
                size="sm"
                className="gap-1.5"
                onClick={handleRefresh}
                disabled={isFetching}
              >
                <RefreshCw
                  className={`size-3.5 ${isFetching ? 'animate-spin' : ''}`}
                />
                <span>Refresh</span>
              </Button>
            </div>
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
                <p className="text-sm font-medium text-foreground">
                  Monitoring not enabled
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  Enable monitoring on the service page to start collecting
                  metrics.
                </p>
              </div>
              <Button
                variant="outline"
                onClick={() => navigate(`/storage/${id}`)}
              >
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
                <p className="text-sm font-medium text-foreground">
                  Collecting first metrics…
                </p>
                <p className="text-sm text-muted-foreground">
                  First metrics appear within 30 seconds.
                </p>
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
    </RefreshIntervalContext.Provider>
  )
}
