import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ClusterDnsCard } from '@/components/settings/ClusterDnsCard'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  adminListNodesOptions,
  adminGetNodeOptions,
  adminListNodeContainersOptions,
  getJoinTokenStatusOptions,
  generateJoinTokenMutation,
  revokeJoinTokenMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  NodeInfoResponse,
  NodeContainerResponse,
} from '@/api/client/types.gen'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  Box,
  ChevronDown,
  ChevronRight,
  Copy,
  Cpu,
  ExternalLink,
  Globe,
  HardDrive,
  Key,
  Loader2,
  MemoryStick,
  Pause,
  Play,
  RefreshCw,
  Server,
  Shield,
  Tag,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
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
import { toast } from 'sonner'
import { client } from '@/api/client/client.gen'

// ── Helpers ──

function StatusBadge({ status }: { status: string }) {
  const styles: Record<string, string> = {
    active:
      'bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20',
    offline: 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/20',
    draining:
      'bg-orange-500/15 text-orange-700 dark:text-orange-400 border-orange-500/20',
    drained:
      'bg-gray-500/15 text-gray-700 dark:text-gray-400 border-gray-500/20',
    pending:
      'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 border-yellow-500/20',
  }
  return (
    <Badge
      variant="default"
      className={`${styles[status] ?? ''} text-xs capitalize`}
    >
      {status}
    </Badge>
  )
}

function formatRelativeTime(dateStr: string | null | undefined): string {
  if (!dateStr) return 'Never'
  const date = new Date(dateStr)
  const now = new Date()
  const diffMs = now.getTime() - date.getTime()
  const diffSecs = Math.floor(diffMs / 1000)

  if (diffSecs < 60) return `${diffSecs}s ago`
  const diffMins = Math.floor(diffSecs / 60)
  if (diffMins < 60) return `${diffMins}m ago`
  const diffHours = Math.floor(diffMins / 60)
  if (diffHours < 24) return `${diffHours}h ago`
  const diffDays = Math.floor(diffHours / 24)
  return `${diffDays}d ago`
}

function CopyButton({ text }: { text: string }) {
  const handleCopy = () => {
    navigator.clipboard.writeText(text)
    toast.success('Copied to clipboard')
  }

  return (
    <Button
      variant="ghost"
      size="icon"
      className="h-6 w-6 shrink-0"
      onClick={handleCopy}
    >
      <Copy className="h-3 w-3" />
    </Button>
  )
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const k = 1024
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  return `${(bytes / Math.pow(k, i)).toFixed(1)} ${sizes[i]}`
}

function formatPercent(value: number): string {
  return `${value.toFixed(1)}%`
}

// ── Mini usage bar ──

function UsageBar({
  percent,
  label,
}: {
  percent: number
  label: string
}) {
  const color =
    percent > 90
      ? 'bg-red-500'
      : percent > 70
        ? 'bg-amber-500'
        : 'bg-green-500'
  return (
    <div className="flex items-center gap-1.5" title={label}>
      <div className="w-12 h-1.5 bg-muted rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full ${color}`}
          style={{ width: `${Math.min(percent, 100)}%` }}
        />
      </div>
      <span className="text-[10px] text-muted-foreground w-8 tabular-nums">
        {formatPercent(percent)}
      </span>
    </div>
  )
}

// ── Capacity metrics display ──

interface CapacityMetrics {
  cpu_percent?: number
  memory_used_bytes?: number
  memory_total_bytes?: number
  disk_used_bytes?: number
  disk_total_bytes?: number
}

function parseCapacity(capacity: unknown): CapacityMetrics | null {
  if (!capacity || typeof capacity !== 'object') return null
  const c = capacity as Record<string, unknown>
  if (!c.cpu_percent && !c.memory_total_bytes) return null
  return {
    cpu_percent: typeof c.cpu_percent === 'number' ? c.cpu_percent : undefined,
    memory_used_bytes:
      typeof c.memory_used_bytes === 'number' ? c.memory_used_bytes : undefined,
    memory_total_bytes:
      typeof c.memory_total_bytes === 'number'
        ? c.memory_total_bytes
        : undefined,
    disk_used_bytes:
      typeof c.disk_used_bytes === 'number' ? c.disk_used_bytes : undefined,
    disk_total_bytes:
      typeof c.disk_total_bytes === 'number' ? c.disk_total_bytes : undefined,
  }
}

function NodeCapacityMini({ capacity }: { capacity: unknown }) {
  const metrics = parseCapacity(capacity)
  if (!metrics) {
    return (
      <span className="text-xs text-muted-foreground italic">No metrics</span>
    )
  }

  const memPercent =
    metrics.memory_used_bytes && metrics.memory_total_bytes
      ? (metrics.memory_used_bytes / metrics.memory_total_bytes) * 100
      : 0
  const diskPercent =
    metrics.disk_used_bytes && metrics.disk_total_bytes
      ? (metrics.disk_used_bytes / metrics.disk_total_bytes) * 100
      : 0

  return (
    <div className="flex flex-col gap-0.5">
      {metrics.cpu_percent !== undefined && (
        <UsageBar percent={metrics.cpu_percent} label={`CPU: ${formatPercent(metrics.cpu_percent)}`} />
      )}
      {metrics.memory_total_bytes && (
        <UsageBar percent={memPercent} label={`Memory: ${formatPercent(memPercent)}`} />
      )}
      {metrics.disk_total_bytes && (
        <UsageBar percent={diskPercent} label={`Disk: ${formatPercent(diskPercent)}`} />
      )}
    </div>
  )
}

// ── Architecture display ──

/**
 * A node reports its container platform (`linux/amd64`, `linux/arm64`) on every
 * heartbeat. "Unknown" means an agent that predates multi-arch support, or one
 * that hasn't heartbeated since upgrading — worth surfacing, since the
 * scheduler cannot verify image compatibility for those nodes.
 */
function NodeArchitecture({ architecture }: { architecture?: string | null }) {
  if (!architecture) {
    return (
      <span
        className="text-xs text-muted-foreground"
        title="This node has not reported its architecture yet"
      >
        Unknown
      </span>
    )
  }
  // Drop the redundant `linux/` prefix — every Temps node runs Linux.
  const short = architecture.replace(/^linux\//, '')
  return (
    <Badge
      variant="outline"
      className="font-mono text-[10px]"
      title={architecture}
    >
      {short}
    </Badge>
  )
}

// ── Labels display ──

function NodeLabels({ labels }: { labels: unknown }) {
  if (!labels || typeof labels !== 'object') return null
  const entries = Object.entries(labels as Record<string, unknown>)
  if (entries.length === 0) return null

  return (
    <div className="flex flex-wrap gap-1">
      {entries.map(([key, value]) => (
        <Badge
          key={key}
          variant="outline"
          className="text-[10px] font-mono px-1.5 py-0"
        >
          {key}={String(value)}
        </Badge>
      ))}
    </div>
  )
}

// ── Join Token Section ──

function JoinTokenSection() {
  const queryClient = useQueryClient()
  const { data: tokenStatus, isLoading: statusLoading } = useQuery({
    ...getJoinTokenStatusOptions(),
  })
  const generateToken = useMutation({
    ...generateJoinTokenMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getJoinTokenStatusOptions().queryKey,
      })
    },
  })
  const revokeToken = useMutation({
    ...revokeJoinTokenMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getJoinTokenStatusOptions().queryKey,
      })
    },
  })
  const [generatedToken, setGeneratedToken] = useState<string | null>(null)

  const externalUrl = window.location.origin

  const handleGenerate = async () => {
    try {
      const result = await generateToken.mutateAsync({})
      setGeneratedToken(result.token)
      toast.success('Join token generated')
    } catch {
      toast.error('Failed to generate join token')
    }
  }

  const handleRevoke = async () => {
    try {
      await revokeToken.mutateAsync({})
      setGeneratedToken(null)
      toast.success('Join token revoked')
    } catch {
      toast.error('Failed to revoke join token')
    }
  }

  if (statusLoading) {
    return (
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        Loading token status...
      </div>
    )
  }

  const hasToken = tokenStatus?.has_token ?? false

  if (generatedToken) {
    const joinCommand = `temps join ${externalUrl} ${generatedToken} --private-address <worker-ip>`
    return (
      <div className="space-y-4">
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            Save this token now
          </AlertTitle>
          <AlertDescription className="text-amber-600 dark:text-amber-300">
            This is the only time the join token will be displayed. Copy the
            command below and store the token securely.
          </AlertDescription>
        </Alert>

        <JoinInstructions joinCommand={joinCommand} />

        <div className="flex items-center gap-2">
          <Button
            variant="destructive"
            size="sm"
            onClick={handleRevoke}
            disabled={revokeToken.isPending}
          >
            {revokeToken.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-1" />
            ) : (
              <Trash2 className="h-4 w-4 mr-1" />
            )}
            Revoke Token
          </Button>
        </div>
      </div>
    )
  }

  if (hasToken) {
    const joinCommand = `temps join ${externalUrl} <join-token> --private-address <worker-ip>`
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-2 text-sm">
          <Badge
            variant="default"
            className="bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20"
          >
            <Shield className="h-3 w-3 mr-1" />
            Token configured
          </Badge>
          <span className="text-muted-foreground">
            Node registration requires a valid join token.
          </span>
        </div>

        <JoinInstructions joinCommand={joinCommand} />

        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={handleGenerate}
            disabled={generateToken.isPending}
          >
            {generateToken.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-1" />
            ) : (
              <RefreshCw className="h-4 w-4 mr-1" />
            )}
            Regenerate Token
          </Button>
          <Button
            variant="destructive"
            size="sm"
            onClick={handleRevoke}
            disabled={revokeToken.isPending}
          >
            {revokeToken.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-1" />
            ) : (
              <Trash2 className="h-4 w-4 mr-1" />
            )}
            Revoke Token
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <Alert className="border-amber-500/30 bg-amber-500/5">
        <AlertTriangle className="h-4 w-4 text-amber-500" />
        <AlertTitle className="text-amber-700 dark:text-amber-400">
          No join token configured
        </AlertTitle>
        <AlertDescription className="text-amber-600 dark:text-amber-300">
          Without a join token, any machine that knows the endpoint can register
          as a worker node. Generate a token to secure node registration.
        </AlertDescription>
      </Alert>

      <Button onClick={handleGenerate} disabled={generateToken.isPending}>
        {generateToken.isPending ? (
          <Loader2 className="h-4 w-4 animate-spin mr-1" />
        ) : (
          <Key className="h-4 w-4 mr-1" />
        )}
        Generate Join Token
      </Button>
    </div>
  )
}

function JoinInstructions({ joinCommand }: { joinCommand: string }) {
  const [expanded, setExpanded] = useState(true)

  return (
    <div className="rounded-lg border bg-muted/30 p-4">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-2 text-sm font-medium w-full text-left"
      >
        {expanded ? (
          <ChevronDown className="h-4 w-4" />
        ) : (
          <ChevronRight className="h-4 w-4" />
        )}
        How to add a worker node
      </button>
      {expanded && (
        <div className="mt-3 space-y-3 text-sm text-muted-foreground">
          <div>
            <p className="font-medium text-foreground">
              1. Install Temps CLI on the worker machine
            </p>
            <div className="mt-1 flex items-center gap-2 rounded-md bg-muted px-3 py-2 font-mono text-xs">
              <span className="flex-1 overflow-x-auto">
                curl -fsSL https://temps.sh/install.sh | bash
              </span>
              <CopyButton text="curl -fsSL https://temps.sh/install.sh | bash" />
            </div>
          </div>
          <div>
            <p className="font-medium text-foreground">2. Join the cluster</p>
            <div className="mt-1 flex items-center gap-2 rounded-md bg-muted px-3 py-2 font-mono text-xs">
              <span className="flex-1 overflow-x-auto">{joinCommand}</span>
              <CopyButton text={joinCommand} />
            </div>
            <p className="mt-1 text-xs">
              Replace <code>&lt;worker-ip&gt;</code> with the worker machine's
              private IP address.
            </p>
          </div>
          <div>
            <p className="font-medium text-foreground">3. Start the agent</p>
            <div className="mt-1 flex items-center gap-2 rounded-md bg-muted px-3 py-2 font-mono text-xs">
              <span className="flex-1 overflow-x-auto">temps agent</span>
              <CopyButton text="temps agent" />
            </div>
            <p className="mt-1 text-xs">
              Reads config saved by <code>temps join</code> and starts the
              worker with heartbeats.
            </p>
          </div>
          <div>
            <a
              href="https://temps.sh/docs/multi-node"
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline"
            >
              Full documentation
              <ExternalLink className="h-3 w-3" />
            </a>
          </div>
        </div>
      )}
    </div>
  )
}

// ── Node Table ──

function NodeTable({
  nodes,
}: {
  nodes: NodeInfoResponse[]
}) {
  const navigate = useNavigate()
  return (
    <div className="overflow-x-auto">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Status</TableHead>
            <TableHead className="hidden sm:table-cell">Arch</TableHead>
            <TableHead className="hidden md:table-cell">Labels</TableHead>
            <TableHead className="hidden lg:table-cell">Resources</TableHead>
            <TableHead className="hidden md:table-cell">Address</TableHead>
            <TableHead>Last Heartbeat</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {nodes.map((node) => (
            <TableRow
              key={node.id}
              className="cursor-pointer hover:bg-accent/50"
              onClick={() => navigate(`/settings/nodes/${node.id}`)}
            >
              <TableCell>
                <div className="flex items-center gap-2">
                  <Server className="h-4 w-4 text-muted-foreground shrink-0" />
                  <div className="min-w-0">
                    <span className="font-medium truncate max-w-[200px] block">
                      {node.name}
                    </span>
                    <Badge
                      variant="outline"
                      className="text-[10px] capitalize mt-0.5"
                    >
                      {node.role}
                    </Badge>
                  </div>
                </div>
              </TableCell>
              <TableCell>
                <StatusBadge status={node.status} />
              </TableCell>
              <TableCell className="hidden sm:table-cell">
                <NodeArchitecture architecture={node.architecture} />
              </TableCell>
              <TableCell className="hidden md:table-cell">
                <NodeLabels labels={node.labels} />
              </TableCell>
              <TableCell className="hidden lg:table-cell">
                <NodeCapacityMini capacity={node.capacity} />
              </TableCell>
              <TableCell className="hidden md:table-cell">
                <span className="font-mono text-xs text-muted-foreground truncate max-w-[200px] block">
                  {node.private_address}
                </span>
              </TableCell>
              <TableCell>
                <span className="text-sm text-muted-foreground">
                  {formatRelativeTime(node.last_heartbeat)}
                </span>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}

// ── Metric Card ──

function MetricCard({
  icon,
  label,
  value,
  percent,
}: {
  icon: React.ReactNode
  label: string
  value: string
  percent: number
}) {
  const barColor =
    percent > 90 ? 'bg-red-500' : percent > 70 ? 'bg-amber-500' : 'bg-green-500'
  return (
    <Card>
      <CardContent className="pt-4 pb-3 px-4">
        <div className="flex items-center justify-between mb-2">
          <div className="flex items-center gap-2 text-sm font-medium">
            {icon}
            {label}
          </div>
          <span className="text-sm font-mono">{value}</span>
        </div>
        <div className="w-full h-2 bg-muted rounded-full overflow-hidden">
          <div
            className={`h-full rounded-full transition-all ${barColor}`}
            style={{ width: `${Math.min(percent, 100)}%` }}
          />
        </div>
      </CardContent>
    </Card>
  )
}

function NodeDetailLabels({ labels }: { labels: unknown }) {
  if (!labels || typeof labels !== 'object') return null
  const entries = Object.entries(labels as Record<string, unknown>)
  if (entries.length === 0) return null

  return (
    <Card>
      <CardHeader className="py-3 px-4">
        <CardTitle className="text-sm flex items-center gap-2">
          <Tag className="h-4 w-4" />
          Labels
        </CardTitle>
      </CardHeader>
      <CardContent className="px-4 pb-3">
        <div className="flex flex-wrap gap-1.5">
          {entries.map(([key, value]) => (
            <Badge key={key} variant="secondary" className="text-xs font-mono">
              {key}={String(value)}
            </Badge>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

// ── Edge Analytics Section ──

interface EdgeOverview {
  total_requests: number
  cache_hits: number
  cache_misses: number
  cache_bypasses: number
  cache_hit_rate: number
  bytes_from_cache: number
  bytes_from_origin: number
  bandwidth_savings_rate: number
  avg_origin_latency_ms: number
  unique_domains: number
}

function EdgeAnalyticsSection({ nodeId }: { nodeId: number }) {
  interface EdgeProxyResponse {
    node_id: number
    node_name: string
    region: string
    data: EdgeOverview
  }

  const { data, isLoading } = useQuery<EdgeProxyResponse[]>({
    queryKey: ['edge-analytics-overview', nodeId],
    queryFn: async () => {
      const resp = await client.get({
        url: '/internal/edge/analytics/overview' as never,
        query: { node_id: nodeId },
      })
      return resp.data as EdgeProxyResponse[]
    },
    refetchInterval: 15_000,
  })

  const overview = data?.[0]?.data

  return (
    <Card>
      <CardHeader className="py-3 px-4">
        <CardTitle className="text-sm flex items-center gap-2">
          <Globe className="h-4 w-4" />
          Edge Analytics
        </CardTitle>
        <CardDescription>Cache performance and traffic overview</CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="flex items-center justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin" />
          </div>
        ) : !overview ? (
          <p className="text-sm text-muted-foreground text-center py-6">
            No analytics data available. The edge node may be offline or hasn't received traffic yet.
          </p>
        ) : (
          <div className="space-y-4">
            {/* Cache metrics */}
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
              <MiniStat label="Total Requests" value={overview.total_requests.toLocaleString()} />
              <MiniStat label="Cache Hits" value={overview.cache_hits.toLocaleString()} accent="green" />
              <MiniStat label="Cache Misses" value={overview.cache_misses.toLocaleString()} accent="yellow" />
              <MiniStat label="Bypassed" value={overview.cache_bypasses.toLocaleString()} />
            </div>

            {/* Hit rate bar */}
            <div className="space-y-1.5">
              <div className="flex justify-between text-xs">
                <span className="text-muted-foreground">Cache Hit Rate</span>
                <span className="font-medium">{formatPercent(overview.cache_hit_rate * 100)}</span>
              </div>
              <div className="h-2 rounded-full bg-muted overflow-hidden">
                <div
                  className="h-full rounded-full bg-green-500 transition-all duration-500"
                  style={{ width: `${Math.min(overview.cache_hit_rate * 100, 100)}%` }}
                />
              </div>
            </div>

            {/* Bandwidth */}
            <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
              <MiniStat label="Served from Cache" value={formatBytes(overview.bytes_from_cache)} accent="green" />
              <MiniStat label="From Origin" value={formatBytes(overview.bytes_from_origin)} />
              <MiniStat label="Avg Origin Latency" value={`${overview.avg_origin_latency_ms.toFixed(1)}ms`} />
            </div>

            {/* Bandwidth savings */}
            <div className="space-y-1.5">
              <div className="flex justify-between text-xs">
                <span className="text-muted-foreground">Bandwidth Savings</span>
                <span className="font-medium">{formatPercent(overview.bandwidth_savings_rate * 100)}</span>
              </div>
              <div className="h-2 rounded-full bg-muted overflow-hidden">
                <div
                  className="h-full rounded-full bg-blue-500 transition-all duration-500"
                  style={{ width: `${Math.min(overview.bandwidth_savings_rate * 100, 100)}%` }}
                />
              </div>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function MiniStat({
  label,
  value,
  accent,
}: {
  label: string
  value: string
  accent?: 'green' | 'yellow' | 'blue'
}) {
  const accentClass = accent === 'green'
    ? 'text-green-600 dark:text-green-400'
    : accent === 'yellow'
      ? 'text-yellow-600 dark:text-yellow-400'
      : accent === 'blue'
        ? 'text-blue-600 dark:text-blue-400'
        : ''

  return (
    <div className="rounded-lg border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className={`text-lg font-semibold tabular-nums ${accentClass}`}>{value}</p>
    </div>
  )
}

// ── Node Detail Page (URL-routed) ──

export function NodeDetailPage() {
  const { nodeId } = useParams<{ nodeId: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const id = Number(nodeId)

  const { data: nodeData } = useQuery({
    ...adminGetNodeOptions({ path: { node_id: id } }),
    enabled: !isNaN(id),
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Worker Nodes', href: '/settings/nodes' },
      { label: nodeData?.name ?? `Node ${nodeId}` },
    ])
  }, [setBreadcrumbs, nodeData?.name, nodeId])

  usePageTitle(nodeData?.name ?? 'Node Detail')

  if (isNaN(id)) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>Invalid node ID.</AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-6">
      <NodeDetail nodeId={id} onBack={() => navigate('/settings/nodes')} />
    </div>
  )
}

function NodeDetail({
  nodeId,
  onBack,
}: {
  nodeId: number
  onBack: () => void
}) {
  const queryClient = useQueryClient()
  const [showDrainDialog, setShowDrainDialog] = useState(false)
  const [showRemoveDialog, setShowRemoveDialog] = useState(false)
  const [showUndrainDialog, setShowUndrainDialog] = useState(false)
  const [drainPending, setDrainPending] = useState(false)
  const [removePending, setRemovePending] = useState(false)
  const [undrainPending, setUndrainPending] = useState(false)

  const { data: node, isLoading: nodeLoading } = useQuery({
    ...adminGetNodeOptions({ path: { node_id: nodeId } }),
    refetchInterval: 15_000,
  })

  const { data: containersData, isLoading: containersLoading } = useQuery({
    ...adminListNodeContainersOptions({ path: { node_id: nodeId } }),
    refetchInterval: 15_000,
  })

  // Poll drain status when node is draining (every 5s for live progress)
  interface DrainStatus {
    remaining_containers: number
    drain_complete: boolean
    can_remove: boolean
    message: string
  }
  const { data: drainStatus } = useQuery<DrainStatus>({
    queryKey: ['node-drain-status', nodeId],
    queryFn: async () => {
      const resp = await client.get({
        url: '/internal/nodes/{node_id}/drain' as never,
        path: { node_id: nodeId },
      })
      return resp.data as DrainStatus
    },
    enabled: node?.status === 'draining',
    refetchInterval: node?.status === 'draining' ? 5_000 : false,
  })

  const containers = containersData?.containers ?? []
  const metrics = node ? parseCapacity(node.capacity) : null

  const handleDrain = async () => {
    setDrainPending(true)
    try {
      const resp = await client.post({
        url: '/internal/nodes/{node_id}/drain',
        path: { node_id: nodeId },
      })
      if (resp.error) {
        toast.error('Failed to drain node')
        return
      }
      toast.success(`Node is now draining`)
      queryClient.invalidateQueries({ queryKey: adminGetNodeOptions({ path: { node_id: nodeId } }).queryKey })
      queryClient.invalidateQueries({ queryKey: adminListNodesOptions().queryKey })
    } catch {
      toast.error('Failed to drain node')
    } finally {
      setDrainPending(false)
      setShowDrainDialog(false)
    }
  }

  const handleRemove = async () => {
    setRemovePending(true)
    try {
      const resp = await client.delete({
        url: '/internal/nodes/{node_id}',
        path: { node_id: nodeId },
      })
      if (resp.error) {
        toast.error('Failed to remove node')
        return
      }
      toast.success('Node removed')
      queryClient.invalidateQueries({ queryKey: adminListNodesOptions().queryKey })
      onBack()
    } catch {
      toast.error('Failed to remove node')
    } finally {
      setRemovePending(false)
      setShowRemoveDialog(false)
    }
  }

  const handleUndrain = async () => {
    setUndrainPending(true)
    try {
      const resp = await client.delete({
        url: '/internal/nodes/{node_id}/drain' as never,
        path: { node_id: nodeId },
      })
      if (resp.error) {
        toast.error('Failed to undrain node')
        return
      }
      toast.success('Node reactivated')
      queryClient.invalidateQueries({ queryKey: adminGetNodeOptions({ path: { node_id: nodeId } }).queryKey })
      queryClient.invalidateQueries({ queryKey: adminListNodesOptions().queryKey })
    } catch {
      toast.error('Failed to undrain node')
    } finally {
      setUndrainPending(false)
      setShowUndrainDialog(false)
    }
  }

  if (nodeLoading) {
    return (
      <div className="flex items-center justify-center min-h-[300px]">
        <Loader2 className="h-6 w-6 animate-spin" />
      </div>
    )
  }

  if (!node) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>Node not found.</AlertDescription>
      </Alert>
    )
  }

  const canDrain = node.status === 'active'
  const canUndrain = node.status === 'draining' || node.status === 'drained'
  const canRemove = (node.status === 'drained' && (drainStatus?.can_remove ?? containers.length === 0))
    || node.status === 'offline'

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Button variant="ghost" size="icon" onClick={onBack} className="h-8 w-8">
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <Server className="h-5 w-5 text-muted-foreground" />
            <h3 className="text-lg font-semibold truncate">{node.name}</h3>
            <StatusBadge status={node.status} />
            <NodeArchitecture architecture={node.architecture} />
          </div>
          <p className="text-sm text-muted-foreground mt-0.5">
            {node.private_address} &middot; {node.role} &middot; Last
            heartbeat {formatRelativeTime(node.last_heartbeat)}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {canDrain && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => setShowDrainDialog(true)}
              disabled={drainPending}
            >
              {drainPending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              ) : (
                <Pause className="h-4 w-4 mr-1" />
              )}
              <span className="hidden sm:inline">Drain</span>
            </Button>
          )}
          {canUndrain && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => setShowUndrainDialog(true)}
              disabled={undrainPending}
            >
              {undrainPending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              ) : (
                <Play className="h-4 w-4 mr-1" />
              )}
              <span className="hidden sm:inline">Undrain</span>
            </Button>
          )}
          {canRemove && (
            <Button
              variant="destructive"
              size="sm"
              onClick={() => setShowRemoveDialog(true)}
              disabled={removePending}
            >
              {removePending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              ) : (
                <Trash2 className="h-4 w-4 mr-1" />
              )}
              <span className="hidden sm:inline">Remove</span>
            </Button>
          )}
        </div>
      </div>

      {/* Drain confirmation dialog */}
      <AlertDialog open={showDrainDialog} onOpenChange={setShowDrainDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Drain node "{node.name}"?</AlertDialogTitle>
            <AlertDialogDescription>
              This will stop scheduling new containers to this node and redeploy
              existing workloads to other healthy nodes. The node will remain in
              the cluster in a "draining" state until all containers are migrated.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={drainPending}>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={handleDrain} disabled={drainPending}>
              {drainPending && <Loader2 className="h-4 w-4 animate-spin mr-1" />}
              Drain Node
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Remove confirmation dialog */}
      <AlertDialog open={showRemoveDialog} onOpenChange={setShowRemoveDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove node "{node.name}"?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently remove the node from the cluster. This action
              cannot be undone. The node must be drained first (no active containers).
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={removePending}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleRemove}
              disabled={removePending}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {removePending && <Loader2 className="h-4 w-4 animate-spin mr-1" />}
              Remove Node
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Undrain confirmation dialog */}
      <AlertDialog open={showUndrainDialog} onOpenChange={setShowUndrainDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Reactivate node "{node.name}"?</AlertDialogTitle>
            <AlertDialogDescription>
              This will set the node back to "active" so it can accept new container
              deployments again. Any containers that were already migrated off will
              not be moved back automatically.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={undrainPending}>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={handleUndrain} disabled={undrainPending}>
              {undrainPending && <Loader2 className="h-4 w-4 animate-spin mr-1" />}
              Reactivate Node
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Draining/drained status banner */}
      {(node.status === 'draining' || node.status === 'drained') && (
        <Alert className={node.status === 'drained' || drainStatus?.drain_complete ? 'border-green-500/30 bg-green-500/5' : 'border-orange-500/30 bg-orange-500/5'}>
          {node.status === 'drained' || drainStatus?.drain_complete ? (
            <AlertCircle className="h-4 w-4 text-green-500" />
          ) : (
            <Pause className="h-4 w-4 text-orange-500" />
          )}
          <AlertTitle className={node.status === 'drained' || drainStatus?.drain_complete ? 'text-green-700 dark:text-green-400' : 'text-orange-700 dark:text-orange-400'}>
            {node.status === 'drained' ? 'Node is drained' : drainStatus?.drain_complete ? 'Drain complete' : 'Node is draining'}
          </AlertTitle>
          <AlertDescription className={node.status === 'drained' || drainStatus?.drain_complete ? 'text-green-600 dark:text-green-300' : 'text-orange-600 dark:text-orange-300'}>
            {node.status === 'drained'
              ? 'All containers have been migrated. You can remove the node or reactivate it with the Undrain button.'
              : drainStatus
                ? drainStatus.message
                : containers.length > 0
                  ? `${containers.length} container(s) still running. Workloads are being migrated to other nodes.`
                  : 'All containers have been migrated. This node can now be safely removed.'}
          </AlertDescription>
        </Alert>
      )}

      {/* Labels */}
      <NodeDetailLabels labels={node.labels} />

      {/* Metrics */}
      {metrics && (
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          {metrics.cpu_percent !== undefined && (
            <MetricCard
              icon={<Cpu className="h-4 w-4 text-muted-foreground" />}
              label="CPU"
              value={formatPercent(metrics.cpu_percent)}
              percent={metrics.cpu_percent}
            />
          )}

          {metrics.memory_total_bytes != null && metrics.memory_used_bytes != null && (
            <MetricCard
              icon={<MemoryStick className="h-4 w-4 text-muted-foreground" />}
              label="Memory"
              value={`${formatBytes(metrics.memory_used_bytes)} / ${formatBytes(metrics.memory_total_bytes)}`}
              percent={(metrics.memory_used_bytes / metrics.memory_total_bytes) * 100}
            />
          )}

          {metrics.disk_total_bytes != null && metrics.disk_used_bytes != null && (
            <MetricCard
              icon={<HardDrive className="h-4 w-4 text-muted-foreground" />}
              label="Disk"
              value={`${formatBytes(metrics.disk_used_bytes)} / ${formatBytes(metrics.disk_total_bytes)}`}
              percent={(metrics.disk_used_bytes / metrics.disk_total_bytes) * 100}
            />
          )}
        </div>
      )}

      {/* Edge Analytics (only for edge nodes) */}
      {node.role === 'edge' && <EdgeAnalyticsSection nodeId={nodeId} />}

      {/* Containers */}
      <Card>
        <CardHeader className="py-3 px-4">
          <CardTitle className="text-sm flex items-center gap-2">
            <Box className="h-4 w-4" />
            Containers
            {!containersLoading && (
              <Badge variant="secondary" className="text-xs ml-1">
                {containers.length}
              </Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent className="px-0 pb-0">
          {containersLoading ? (
            <div className="flex items-center justify-center py-8">
              <Loader2 className="h-5 w-5 animate-spin" />
            </div>
          ) : containers.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-8 text-center px-4">
              <Box className="h-8 w-8 text-muted-foreground mb-2" />
              <p className="text-sm text-muted-foreground">
                No containers running on this node.
              </p>
            </div>
          ) : (
            <div className="overflow-x-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Container</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead className="hidden md:table-cell">
                      Project
                    </TableHead>
                    <TableHead className="hidden md:table-cell">
                      Environment
                    </TableHead>
                    <TableHead className="hidden lg:table-cell">
                      Image
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {containers.map((c: NodeContainerResponse) => (
                    <TableRow key={c.container_id}>
                      <TableCell>
                        <span className="font-mono text-xs truncate max-w-[200px] block">
                          {c.container_name}
                        </span>
                      </TableCell>
                      <TableCell>
                        <Badge
                          variant={
                            c.status === 'running' ? 'default' : 'secondary'
                          }
                          className={`text-xs ${
                            c.status === 'running'
                              ? 'bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20'
                              : ''
                          }`}
                        >
                          {c.status}
                        </Badge>
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <span className="text-sm">{c.project_name}</span>
                      </TableCell>
                      <TableCell className="hidden md:table-cell">
                        <Badge variant="outline" className="text-xs">
                          {c.environment_name}
                        </Badge>
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <span className="font-mono text-xs text-muted-foreground truncate max-w-[250px] block">
                          {c.image_name}
                        </span>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

// ── Main Page ──

export function NodesPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data, isLoading, error } = useQuery({
    ...adminListNodesOptions(),
    refetchInterval: 30_000,
  })
  const nodes = data?.nodes ?? []

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Worker Nodes' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Worker Nodes')

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>Failed to load worker nodes.</AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Worker Nodes</CardTitle>
          <CardDescription>
            Distribute container deployments across multiple servers. Worker
            nodes run the Temps agent and receive containers from the control
            plane.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          <JoinTokenSection />

          {nodes.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-center border-t pt-6">
              <Server className="h-12 w-12 text-muted-foreground mb-4" />
              <p className="text-sm font-medium">No worker nodes</p>
              <p className="text-sm text-muted-foreground mt-1 max-w-md">
                All deployments run on this server. Add worker nodes to
                distribute containers across multiple machines.
              </p>
            </div>
          ) : (
            <NodeTable nodes={nodes} />
          )}
        </CardContent>
      </Card>

      <ClusterDnsCard />
    </div>
  )
}
