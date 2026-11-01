// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader, SettingsSurface } from '@temps-sdk/ds'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { ClusterDnsCard } from '@/components/settings/ClusterDnsCard'
import { ClusterDnsOffNotice } from '@/components/settings/ClusterDnsOffNotice'
import { ClusterNetworkCard } from '@/components/settings/ClusterNetworkCard'
import { useClusterNetwork } from '@/hooks/useClusterNetwork'
import {
  nodeOverlayStatus,
  type ClusterNetworkNode,
} from '@/lib/cluster-network'
import { AddWorkerNode } from '@/components/nodes/AddWorkerNode'
import { MeshHubCard } from '@/components/nodes/MeshHubCard'
import { RotateClusterCaDialog } from '@/components/nodes/RotateClusterCaDialog'
import { useClusterCaFingerprint } from '@/hooks/useClusterCaFingerprint'
import { QueryErrorAlert } from '@/components/nodes/mesh-ui'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { WorkerNodeRequiredAlert } from '@/components/nodes/WorkerNodeRequiredBanner'
import { BuilderLimitsPanel } from '@/components/nodes/BuilderLimitsPanel'
import { WorkerIngressBadge } from '@/components/nodes/WorkerIngressBadge'
import { WorkerIngressCard } from '@/components/nodes/WorkerIngressCard'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import { workerIngressWay } from '@/lib/worker-ingress'
import { ingressSetupUrl } from '@/components/nodes/ingress-setup'
import { NodeSandboxesPanel } from '@/components/nodes/NodeSandboxesPanel'
import { NodeRoleBadge } from '@/components/nodes/NodeRoleBadge'
import { NodeRowActions } from '@/components/nodes/NodeRowActions'
import { useNodeRemoval } from '@/components/nodes/useNodeRemoval'
import { NODE_ROLE_BADGE, nodeSchedulingRole } from '@/lib/node-role'
import { canManageSandboxPlacement } from '@/components/sandboxes/helpers'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useAuth } from '@/contexts/AuthContext-shared'
import { problemDetail } from '@/lib/api-problem'
import {
  MeshConnectionBadge,
  useWireguardMesh,
  WorkerJoinGuide,
} from '@/components/nodes/WorkerJoinGuide'
import { strandedPublicNodes } from '@/lib/wireguard-mesh'
import {
  useInvalidateNodeCapability,
  useNodeCapability,
} from '@/hooks/useNodeCapability'
import {
  canAddWorkerNode,
  nodeIsUnreachable,
  nodeRemoval,
  nodeRemovalBlocker,
  shouldPromptForFirstWorkerNode,
  WORKER_NODES_URL,
  nodesAdvancedSection,
} from '@/lib/worker-nodes'
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
import { usePlatformFeatures } from '@/hooks/usePlatformFeatures'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import {
  adminListNodesOptions,
  adminDrainStatusOptions,
  adminGetNodeOptions,
  adminListNodeContainersOptions,
  listNodeSandboxesOptions,
  getJoinTokenStatusOptions,
  getSettingsOptions,
  generateJoinTokenMutation,
  revokeJoinTokenMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  NodeInfoResponse,
  NodeContainerResponse,
  WireguardMeshStatusResponse,
} from '@/api/client/types.gen'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  AlertTriangle,
  Box,
  ChevronDown,
  Cpu,
  Globe,
  HardDrive,
  Key,
  Loader2,
  MemoryStick,
  Pause,
  Play,
  Plus,
  RefreshCw,
  Server,
  Shield,
  Tag,
  Terminal,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import {
  Link,
  useLocation,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { ReadFailure } from '@/components/ui/read-failure'
import { isForbiddenRead, isVerifiedNotFound } from '@/lib/read-failure'
import { refetchUnlessRefused } from '@/lib/query-retry'
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
import {
  NodeDesiredStateStream,
  NodeNetworkPolicySettings,
  NodeSchedulingResources,
} from '@/components/nodes/NodeWorkloadSettings'
import { NodeOverviewAlerts } from '@/components/nodes/NodeOverviewAlerts'
import { NodeNetworkStatusCard } from '@/components/nodes/NodeNetworkStatusCard'
import {
  nodeSections,
  nodeSectionUrl,
  resolveNodeSection,
  type NodeSection,
} from '@/lib/node-detail'
import { NodeStateStreamStatus } from '@/components/nodes/NodeStateStreamStatus'
import { NodeAlertRules } from '@/components/monitoring/NodeAlertRules'
import { ServerMonitoring } from '@/components/monitoring/ServerMonitoring'

// ── Helpers ──

/** Status tones, from the theme's contrast-checked status tokens. */
const nodeStatusVariant: Record<
  string,
  'success-subtle' | 'destructive-subtle' | 'warning-subtle' | 'neutral-subtle'
> = {
  active: 'success-subtle',
  offline: 'destructive-subtle',
  draining: 'warning-subtle',
  drained: 'neutral-subtle',
  pending: 'warning-subtle',
}

function StatusBadge({ status }: { status: string }) {
  return (
    <Badge
      variant={nodeStatusVariant[status] ?? 'neutral-subtle'}
      className="text-xs capitalize"
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

function UsageBar({ percent, label }: { percent: number; label: string }) {
  const color =
    percent > 90 ? 'bg-red-500' : percent > 70 ? 'bg-amber-500' : 'bg-green-500'
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
        <UsageBar
          percent={metrics.cpu_percent}
          label={`CPU: ${formatPercent(metrics.cpu_percent)}`}
        />
      )}
      {metrics.memory_total_bytes && (
        <UsageBar
          percent={memPercent}
          label={`Memory: ${formatPercent(memPercent)}`}
        />
      )}
      {metrics.disk_total_bytes && (
        <UsageBar
          percent={diskPercent}
          label={`Disk: ${formatPercent(diskPercent)}`}
        />
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
  const {
    data: tokenStatus,
    isLoading: statusLoading,
    error: statusError,
    refetch: refetchStatus,
    isFetching: statusFetching,
  } = useQuery({
    ...getJoinTokenStatusOptions(),
  })
  const generateToken = useMutation({
    ...generateJoinTokenMutation(),
    meta: { errorTitle: 'Failed to generate join token' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getJoinTokenStatusOptions().queryKey,
      })
      queryClient.invalidateQueries({
        queryKey: getSettingsOptions().queryKey,
      })
    },
  })
  const revokeToken = useMutation({
    ...revokeJoinTokenMutation(),
    meta: { errorTitle: 'Failed to revoke join token' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: getJoinTokenStatusOptions().queryKey,
      })
    },
  })
  const [generatedToken, setGeneratedToken] = useState<string | null>(null)
  const [confirmTokenAction, setConfirmTokenAction] = useState<
    'revoke' | 'regenerate' | null
  >(null)

  const handleGenerate = async () => {
    try {
      const result = await generateToken.mutateAsync({})
      setGeneratedToken(result.token)
      toast.success('Join token generated')
    } catch {
      // The app-wide mutation handler already toasts this failure.
    }
  }

  const handleRevoke = async () => {
    try {
      await revokeToken.mutateAsync({})
      setGeneratedToken(null)
      toast.success('Join token revoked')
    } catch {
      // The app-wide mutation handler already toasts this failure.
    }
  }

  const confirmTokenDialog = (
    <ConfirmDeleteDialog
      open={confirmTokenAction !== null}
      onOpenChange={(open) => {
        if (!open) setConfirmTokenAction(null)
      }}
      resourceType="join token"
      resourceName="for worker nodes"
      verb={confirmTokenAction === 'regenerate' ? 'Regenerate' : 'Revoke'}
      consequences={
        confirmTokenAction === 'regenerate'
          ? 'The current token stops working and a new one is shown once. Machines that have not joined yet must use the new token.'
          : 'Without a join token, any machine that knows this endpoint can register as a worker node until you generate a new one.'
      }
      isPending={revokeToken.isPending || generateToken.isPending}
      onConfirm={() => {
        const action =
          confirmTokenAction === 'regenerate' ? handleGenerate : handleRevoke
        void action().finally(() => setConfirmTokenAction(null))
      }}
    />
  )

  if (statusLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-6 w-80" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  // The guide renders in every branch: pairing and adding a server over SSH
  // do not need a join token, and the mesh onboarding lives in it too.
  if (statusError) {
    return (
      <div className="space-y-4">
        <QueryErrorAlert
          title="Could not read the join token status"
          error={statusError}
          onRetry={() => void refetchStatus()}
          retrying={statusFetching}
        />
        <WorkerJoinGuide token={null} />
      </div>
    )
  }

  const hasToken = tokenStatus?.has_token ?? false

  if (generatedToken) {
    return (
      <div className="space-y-4">
        <Alert className="border-amber-500/30 bg-amber-500/5">
          <AlertTriangle className="h-4 w-4 text-amber-500" />
          <AlertTitle className="text-amber-700 dark:text-amber-400">
            Save this token now
          </AlertTitle>
          <AlertDescription className="text-amber-700 dark:text-amber-300">
            This is the only time the join token will be displayed. Copy the
            command below and store the token securely.
          </AlertDescription>
        </Alert>

        <WorkerJoinGuide token={generatedToken} />

        <div className="flex items-center gap-2">
          <Button
            variant="destructive"
            size="sm"
            onClick={() => setConfirmTokenAction('revoke')}
            disabled={revokeToken.isPending}
          >
            {revokeToken.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-1" />
            ) : (
              <Trash2 className="h-4 w-4 mr-1" />
            )}
            Revoke token
          </Button>
        </div>
        {confirmTokenDialog}
      </div>
    )
  }

  if (hasToken) {
    return (
      <div className="space-y-4">
        <div className="flex items-center gap-2 text-sm">
          <Badge variant="success-subtle">
            <Shield className="h-3 w-3 mr-1" />
            Token configured
          </Badge>
          <span className="text-muted-foreground">
            Node registration requires a valid join token.
          </span>
        </div>

        <WorkerJoinGuide token={null} />

        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setConfirmTokenAction('regenerate')}
            disabled={generateToken.isPending}
          >
            {generateToken.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-1" />
            ) : (
              <RefreshCw className="h-4 w-4 mr-1" />
            )}
            Regenerate token
          </Button>
          <Button
            variant="destructive"
            size="sm"
            onClick={() => setConfirmTokenAction('revoke')}
            disabled={revokeToken.isPending}
          >
            {revokeToken.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin mr-1" />
            ) : (
              <Trash2 className="h-4 w-4 mr-1" />
            )}
            Revoke token
          </Button>
        </div>
        {confirmTokenDialog}
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
        <AlertDescription className="text-amber-700 dark:text-amber-300">
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
        Generate join token
      </Button>

      <WorkerJoinGuide
        token={null}
        missingToken={{
          onGenerate: () => void handleGenerate(),
          generating: generateToken.isPending,
        }}
      />
    </div>
  )
}

/** One line under the node detail header saying what its role changes. */
function NodeRoleDescription({ labels }: { labels: unknown }) {
  const role = nodeSchedulingRole(labels)
  if (!role) return null
  return (
    <p className="text-xs text-muted-foreground mt-1">
      {NODE_ROLE_BADGE[role].description}
    </p>
  )
}

// ── Node Table ──

/** A node's cross-node overlay (ADR 056); the cause is in the title. */
function OverlayBadge({ node }: { node: ClusterNetworkNode | undefined }) {
  if (!node) {
    return <span className="text-xs text-muted-foreground">—</span>
  }
  const view = nodeOverlayStatus(node)
  const variant =
    view.tone === 'ok'
      ? 'success-subtle'
      : view.tone === 'warn'
        ? 'warning-subtle'
        : 'neutral-subtle'
  return (
    <Badge variant={variant} className="text-xs" title={view.description}>
      {view.tone === 'warn' ? 'Unavailable' : view.label}
    </Badge>
  )
}

function NodeTable({
  nodes,
  mesh,
  overlay,
}: {
  nodes: NodeInfoResponse[]
  mesh: WireguardMeshStatusResponse | undefined
  overlay: ClusterNetworkNode[] | undefined
}) {
  const navigate = useNavigate()
  const overlayNodes = new Map(overlay?.map((node) => [node.id, node]))
  const showOverlay = overlay?.some((node) => node.compute_cidr) ?? false
  const showMesh = mesh !== undefined && mesh.state !== 'disabled'
  const meshNodes = new Map(mesh?.nodes.map((node) => [node.node_id, node]))
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
            <TableHead className="hidden sm:table-cell">Ingress</TableHead>
            {showMesh && <TableHead>Mesh</TableHead>}
            {showOverlay && <TableHead>Cross-node</TableHead>}
            <TableHead>Last heartbeat</TableHead>
            <TableHead className="text-right">
              <span className="sr-only">Actions</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {nodes.map((node) => {
            const meshNode = meshNodes.get(node.id)
            return (
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
                      <div className="flex flex-wrap items-center gap-1 mt-0.5">
                        <Badge
                          variant="outline"
                          className="text-[10px] capitalize"
                        >
                          {node.role}
                        </Badge>
                        <NodeRoleBadge labels={node.labels} />
                      </div>
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
                <TableCell className="hidden sm:table-cell">
                  <WorkerIngressBadge node={node} />
                </TableCell>
                {showMesh && (
                  <TableCell>
                    {meshNode ? (
                      <MeshConnectionBadge
                        connection={meshNode.connection}
                        address={meshNode.mesh_address}
                        checks={meshNode.checks}
                      />
                    ) : node.role === 'control-plane' && mesh?.control_plane ? (
                      <div>
                        <Badge
                          variant="outline"
                          className="text-xs"
                          title={
                            mesh.control_plane.endpoint
                              ? `Nodes dial it at ${mesh.control_plane.endpoint}`
                              : 'No public endpoint: it dials the nodes that have one'
                          }
                        >
                          {mesh.control_plane.endpoint
                            ? 'Reachable'
                            : 'Dials out'}
                        </Badge>
                        <span className="mt-0.5 block font-mono text-xs text-muted-foreground">
                          {mesh.control_plane.address}
                        </span>
                      </div>
                    ) : (
                      <span className="text-xs text-muted-foreground">—</span>
                    )}
                  </TableCell>
                )}
                {showOverlay && (
                  <TableCell>
                    <OverlayBadge node={overlayNodes.get(node.id)} />
                  </TableCell>
                )}
                <TableCell>
                  <span className="text-sm text-muted-foreground">
                    {formatRelativeTime(node.last_heartbeat)}
                  </span>
                </TableCell>
                <TableCell className="text-right">
                  {node.role !== 'control-plane' && (
                    <NodeRowActions node={node} />
                  )}
                </TableCell>
              </TableRow>
            )
          })}
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
          Edge analytics
        </CardTitle>
        <CardDescription>
          Cache performance and traffic overview
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="flex items-center justify-center py-6">
            <Loader2 className="h-5 w-5 animate-spin" />
          </div>
        ) : !overview ? (
          <p className="text-sm text-muted-foreground text-center py-6">
            No analytics data available. The edge node may be offline or hasn’t
            received traffic yet.
          </p>
        ) : (
          <div className="space-y-4">
            {/* Cache metrics */}
            <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
              <MiniStat
                label="Total requests"
                value={overview.total_requests.toLocaleString()}
              />
              <MiniStat
                label="Cache hits"
                value={overview.cache_hits.toLocaleString()}
                accent="green"
              />
              <MiniStat
                label="Cache misses"
                value={overview.cache_misses.toLocaleString()}
                accent="yellow"
              />
              <MiniStat
                label="Bypassed"
                value={overview.cache_bypasses.toLocaleString()}
              />
            </div>

            {/* Hit rate bar */}
            <div className="space-y-1.5">
              <div className="flex justify-between text-xs">
                <span className="text-muted-foreground">Cache Hit Rate</span>
                <span className="font-medium">
                  {formatPercent(overview.cache_hit_rate * 100)}
                </span>
              </div>
              <div className="h-2 rounded-full bg-muted overflow-hidden">
                <div
                  className="h-full rounded-full bg-green-500 transition-all duration-500"
                  style={{
                    width: `${Math.min(overview.cache_hit_rate * 100, 100)}%`,
                  }}
                />
              </div>
            </div>

            {/* Bandwidth */}
            <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
              <MiniStat
                label="Served from cache"
                value={formatBytes(overview.bytes_from_cache)}
                accent="green"
              />
              <MiniStat
                label="From origin"
                value={formatBytes(overview.bytes_from_origin)}
              />
              <MiniStat
                label="Avg origin latency"
                value={`${overview.avg_origin_latency_ms.toFixed(1)}ms`}
              />
            </div>

            {/* Bandwidth savings */}
            <div className="space-y-1.5">
              <div className="flex justify-between text-xs">
                <span className="text-muted-foreground">Bandwidth Savings</span>
                <span className="font-medium">
                  {formatPercent(overview.bandwidth_savings_rate * 100)}
                </span>
              </div>
              <div className="h-2 rounded-full bg-muted overflow-hidden">
                <div
                  className="h-full rounded-full bg-blue-500 transition-all duration-500"
                  style={{
                    width: `${Math.min(overview.bandwidth_savings_rate * 100, 100)}%`,
                  }}
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
  const accentClass =
    accent === 'green'
      ? 'text-success-strong'
      : accent === 'yellow'
        ? 'text-yellow-700 dark:text-yellow-400'
        : accent === 'blue'
          ? 'text-blue-600 dark:text-blue-400'
          : ''

  return (
    <div className="rounded-lg border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className={`text-lg font-semibold tabular-nums ${accentClass}`}>
        {value}
      </p>
    </div>
  )
}

// ── Node Detail Page (URL-routed) ──

export function NodeDetailPage() {
  const { nodeId, section: segment } = useParams<{
    nodeId: string
    section?: string
  }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const id = Number(nodeId)

  const { data: nodeData } = useQuery({
    ...adminGetNodeOptions({ path: { node_id: id } }),
    enabled: !isNaN(id),
  })
  const section = resolveNodeSection(segment, nodeData?.role)
  const sectionTitle = nodeSections(nodeData?.role).find(
    (link) => link.section === section
  )?.title

  useEffect(() => {
    const name = nodeData?.name ?? `Node ${nodeId}`
    setBreadcrumbs(
      section === 'overview'
        ? [{ label: 'Worker nodes', href: WORKER_NODES_URL }, { label: name }]
        : [
            { label: 'Worker nodes', href: WORKER_NODES_URL },
            { label: name, href: nodeSectionUrl(id, 'overview') },
            { label: sectionTitle ?? section },
          ]
    )
  }, [setBreadcrumbs, nodeData?.name, nodeId, id, section, sectionTitle])

  usePageTitle(
    nodeData?.name
      ? section === 'overview'
        ? nodeData.name
        : `${sectionTitle} · ${nodeData.name}`
      : 'Node Detail'
  )

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
    <div className="w-full space-y-6">
      <NodeDetail
        nodeId={id}
        section={section}
        onBack={() => navigate('/settings/nodes')}
      />
    </div>
  )
}

function NodeDetail({
  nodeId,
  section,
  onBack,
}: {
  nodeId: number
  section: NodeSection
  onBack: () => void
}) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  // Draining, removing or reactivating a node changes what this installation
  // can schedule, so the capability every page's banner reads is stale the
  // moment one of those succeeds.
  const invalidateCapability = useInvalidateNodeCapability()
  const [showDrainDialog, setShowDrainDialog] = useState(false)
  const [showUndrainDialog, setShowUndrainDialog] = useState(false)
  const [drainPending, setDrainPending] = useState(false)
  const [undrainPending, setUndrainPending] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const {
    data: node,
    isLoading: nodeLoading,
    error: nodeError,
    refetch: refetchNode,
    isFetching: nodeFetching,
  } = useQuery({
    ...adminGetNodeOptions({ path: { node_id: nodeId } }),
    refetchInterval: 15_000,
  })

  const { data: containersData, isLoading: containersLoading } = useQuery({
    ...adminListNodeContainersOptions({ path: { node_id: nodeId } }),
    refetchInterval: 15_000,
  })

  // Sandboxes are tracked apart from deployment containers (ADR-048). Only
  // admins may list other users' sandboxes, so others see an explanation.
  const { user } = useAuth()
  const canSeeSandboxes = canManageSandboxPlacement(user?.role)
  // A server that does not serve the sandbox API is not asked (its answer
  // would be a bare 404); the panel says why instead. Unknown features
  // (loading, or a server that predates them) still ask, as before.
  const platformFeatures = usePlatformFeatures()
  const sandboxesAvailable = platformFeatures.data?.sandboxes !== false
  const [sandboxPage, setSandboxPage] = useState(1)
  const sandboxesQuery = useQuery({
    ...listNodeSandboxesOptions({
      path: { node: String(nodeId) },
      query: { page: sandboxPage, page_size: 20 },
    }),
    enabled: canSeeSandboxes && sandboxesAvailable,
    refetchInterval: 15_000,
  })
  const liveSandboxes = sandboxesQuery.data?.total ?? 0
  const [searchParams, setSearchParams] = useSearchParams()
  const tab =
    searchParams.get('tab') === 'sandboxes' ? 'sandboxes' : 'containers'
  // The remove flow, shared with the node list's row action.
  const {
    requestRemoval,
    pending: removePending,
    dialog: removeDialog,
  } = useNodeRemoval({
    nodeId,
    nodeName: node?.name ?? '',
    onRemoved: onBack,
    // Live sandboxes block removal and draining doesn't move them: point
    // straight at the tab that can destroy them.
    onShowSandboxes: () =>
      navigate(`${nodeSectionUrl(nodeId, 'overview')}?tab=sandboxes`),
  })

  // Drain status decides whether the node can be removed; the server folds
  // in live sandboxes, which draining does not move. Poll every 5s while
  // draining for live progress.
  const removalRelevant =
    node?.status === 'draining' ||
    node?.status === 'drained' ||
    node?.status === 'offline'
  const { data: drainStatus } = useQuery({
    ...adminDrainStatusOptions({ path: { node_id: nodeId } }),
    enabled: removalRelevant,
    refetchInterval: node?.status === 'draining' ? 5_000 : 15_000,
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
        if (handleSensitiveActionError(resp.error, () => handleDrain())) {
          return
        }
        toast.error('Could not drain node', {
          description: problemDetail(
            resp.error,
            'Check your permissions and try again.'
          ),
        })
        return
      }
      toast.success(`Node is now draining`)
      queryClient.invalidateQueries({
        queryKey: adminGetNodeOptions({ path: { node_id: nodeId } }).queryKey,
      })
      queryClient.invalidateQueries({
        queryKey: adminListNodesOptions().queryKey,
      })
      invalidateCapability()
    } catch {
      toast.error('Failed to drain node')
    } finally {
      setDrainPending(false)
      setShowDrainDialog(false)
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
      queryClient.invalidateQueries({
        queryKey: adminGetNodeOptions({ path: { node_id: nodeId } }).queryKey,
      })
      queryClient.invalidateQueries({
        queryKey: adminListNodesOptions().queryKey,
      })
      invalidateCapability()
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
    // Only a verified 404 says the node is gone; any other failed read is
    // reported as what it is, with a way to replay it.
    return (
      <div className="space-y-4">
        {nodeError && !isVerifiedNotFound(nodeError) ? (
          <ReadFailure
            resource="Node"
            error={nodeError}
            onRetry={() => void refetchNode()}
            retrying={nodeFetching}
          />
        ) : (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Node not found</AlertTitle>
            <AlertDescription>
              No worker node has ID {nodeId}. It may have been removed.
            </AlertDescription>
          </Alert>
        )}
        <Button variant="outline" size="sm" onClick={onBack}>
          Back to worker nodes
        </Button>
      </div>
    )
  }

  const canDrain = node.status === 'active'
  const canUndrain = node.status === 'draining' || node.status === 'drained'
  // What the server reports is on the node decides how it can be removed
  // (ADR 060): an empty node in one step, one with workloads after a drain.
  const removal = nodeRemoval(node)
  const removalBlocker = nodeRemovalBlocker(removal)
  // Draining does not move sandboxes; they have to be destroyed first.
  const remainingSandboxes =
    removal.kind === 'sandboxes'
      ? removal.sandboxes
      : (drainStatus?.remaining_sandboxes ?? liveSandboxes)
  // A server that reports no counts: decide from the drain status as
  // before. A node the control plane stopped hearing from can be removed
  // whatever its status: a drain cannot finish without its agent (ADR 057).
  const legacyRemovableState =
    removal.kind === 'unknown' &&
    ((node.status === 'drained' &&
      (drainStatus?.remaining_containers ?? containers.length) === 0) ||
      node.status === 'offline' ||
      nodeIsUnreachable(node))
  const canRemove =
    removal.kind === 'remove' ||
    (legacyRemovableState && remainingSandboxes === 0)
  const blockedBySandboxes =
    removal.kind === 'sandboxes' ||
    (legacyRemovableState && remainingSandboxes > 0)

  return (
    <div className="space-y-6">
      {verificationDialog}
      {/* Header */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
        <div className="flex-1 min-w-0">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <Server className="h-5 w-5 text-muted-foreground" />
            <h3 className="max-w-full min-w-0 text-lg font-semibold truncate">
              {node.name}
            </h3>
            <StatusBadge status={node.status} />
            <NodeArchitecture architecture={node.architecture} />
            <NodeRoleBadge labels={node.labels} />
            <WorkerIngressBadge node={node} />
          </div>
          <p className="text-sm text-muted-foreground mt-0.5">
            {node.private_address} &middot; {node.role} &middot; Last heartbeat{' '}
            {formatRelativeTime(node.last_heartbeat)}
          </p>
          <NodeRoleDescription labels={node.labels} />
        </div>
        <div className="flex flex-wrap items-center gap-2 shrink-0">
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
              <span className="max-sm:sr-only">Drain</span>
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
              <span className="max-sm:sr-only">Undrain</span>
            </Button>
          )}
          {removal.kind === 'drain-first' && removalBlocker && (
            <span className="text-xs text-muted-foreground">
              {removalBlocker}
            </span>
          )}
          {canRemove && (
            <Button
              variant="destructive"
              size="sm"
              onClick={requestRemoval}
              disabled={removePending}
            >
              {removePending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              ) : (
                <Trash2 className="h-4 w-4 mr-1" />
              )}
              <span className="max-sm:sr-only">Remove</span>
            </Button>
          )}
        </div>
      </div>

      {/* Drain confirmation dialog */}
      <ConfirmDeleteDialog
        open={showDrainDialog}
        onOpenChange={setShowDrainDialog}
        resourceType="node"
        resourceName={node.name}
        verb="Drain"
        confirmLabel="Drain node"
        irreversible={false}
        consequences="This stops scheduling new containers to this node and redeploys existing workloads to other healthy nodes. The node stays in the cluster in a “draining” state until all containers are migrated."
        isPending={drainPending}
        onConfirm={handleDrain}
      />

      {/* Remove confirmation and refusal dialogs */}
      {removeDialog}

      {/* Undrain confirmation dialog */}
      <AlertDialog open={showUndrainDialog} onOpenChange={setShowUndrainDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Reactivate node “{node.name}”?</AlertDialogTitle>
            <AlertDialogDescription>
              This will set the node back to “active” so it can accept new
              container deployments again. Any containers that were already
              migrated off will not be moved back automatically.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={undrainPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={handleUndrain}
              disabled={undrainPending}
            >
              {undrainPending && (
                <Loader2 className="h-4 w-4 animate-spin mr-1" />
              )}
              Reactivate node
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Draining/drained status banner */}
      {(node.status === 'draining' || node.status === 'drained') && (
        <Alert
          className={
            node.status === 'drained' || drainStatus?.drain_complete
              ? 'border-green-500/30 bg-green-500/5'
              : 'border-orange-500/30 bg-orange-500/5'
          }
        >
          {node.status === 'drained' || drainStatus?.drain_complete ? (
            <AlertCircle className="h-4 w-4 text-green-500" />
          ) : (
            <Pause className="h-4 w-4 text-orange-500" />
          )}
          <AlertTitle
            className={
              node.status === 'drained' || drainStatus?.drain_complete
                ? 'text-success-strong'
                : 'text-warning-strong'
            }
          >
            {node.status === 'drained'
              ? 'Node is drained'
              : drainStatus?.drain_complete
                ? 'Drain complete'
                : 'Node is draining'}
          </AlertTitle>
          <AlertDescription
            className={
              node.status === 'drained' || drainStatus?.drain_complete
                ? 'text-success-strong'
                : 'text-warning-strong'
            }
          >
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

      {blockedBySandboxes && (
        <Alert>
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>
            This node still hosts {remainingSandboxes} sandbox(es)
          </AlertTitle>
          <AlertDescription>
            Draining does not move sandboxes, so the node can&rsquo;t be removed
            yet. Destroy them on the{' '}
            <Link
              to={`${nodeSectionUrl(nodeId, 'overview')}?tab=sandboxes`}
              className="underline underline-offset-2"
            >
              Sandboxes tab
            </Link>
            , then remove the node.
          </AlertDescription>
        </Alert>
      )}

      {section === 'overview' && (
        <>
          <NodeOverviewAlerts node={node} />
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

              {metrics.memory_total_bytes != null &&
                metrics.memory_used_bytes != null && (
                  <MetricCard
                    icon={
                      <MemoryStick className="h-4 w-4 text-muted-foreground" />
                    }
                    label="Memory"
                    value={`${formatBytes(metrics.memory_used_bytes)} / ${formatBytes(metrics.memory_total_bytes)}`}
                    percent={
                      (metrics.memory_used_bytes / metrics.memory_total_bytes) *
                      100
                    }
                  />
                )}

              {metrics.disk_total_bytes != null &&
                metrics.disk_used_bytes != null && (
                  <MetricCard
                    icon={
                      <HardDrive className="h-4 w-4 text-muted-foreground" />
                    }
                    label="Disk"
                    value={`${formatBytes(metrics.disk_used_bytes)} / ${formatBytes(metrics.disk_total_bytes)}`}
                    percent={
                      (metrics.disk_used_bytes / metrics.disk_total_bytes) * 100
                    }
                  />
                )}
            </div>
          )}

          {/* Workload on this node: deployment containers and sandboxes */}
          <Tabs
            value={tab}
            onValueChange={(value) =>
              setSearchParams(
                (prev) => {
                  const next = new URLSearchParams(prev)
                  if (value === 'containers') next.delete('tab')
                  else next.set('tab', value)
                  return next
                },
                { replace: true }
              )
            }
          >
            <TabsList>
              <TabsTrigger
                value="containers"
                icon={Box}
                count={containersLoading ? undefined : containers.length}
              >
                Containers
              </TabsTrigger>
              <TabsTrigger
                value="sandboxes"
                icon={Terminal}
                count={sandboxesQuery.data?.total}
              >
                Sandboxes
              </TabsTrigger>
            </TabsList>
            <TabsContent value="containers">
              <Card>
                <CardContent className="px-0 pb-0 pt-0">
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
                              Service
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
                                    c.status === 'running'
                                      ? 'success-subtle'
                                      : 'secondary'
                                  }
                                  className="text-xs"
                                >
                                  {c.status}
                                </Badge>
                              </TableCell>
                              <TableCell className="hidden md:table-cell">
                                <span className="text-sm">
                                  {c.project_name}
                                </span>
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
            </TabsContent>
            <TabsContent value="sandboxes">
              <NodeSandboxesPanel
                nodeId={nodeId}
                nodeName={node.name}
                canSee={canSeeSandboxes}
                query={sandboxesQuery}
                page={sandboxPage}
                onPageChange={setSandboxPage}
                sandboxesAvailable={sandboxesAvailable}
                sandboxesLocal={platformFeatures.data?.sandboxes_local}
              />
            </TabsContent>
          </Tabs>
        </>
      )}

      {section === 'monitoring' && (
        <>
          {/* History and alerts. Always rendered: with no samples yet, or no
              metrics store, ServerMonitoring explains what is missing. */}
          <ServerMonitoring
            nodeId={nodeId}
            embedded
            rangeParamName="metrics_range"
          />
          <NodeAlertRules nodeId={nodeId} />

          {/* Edge Analytics (only for edge nodes) */}
          {node.role === 'edge' && <EdgeAnalyticsSection nodeId={nodeId} />}
        </>
      )}

      {section === 'networking' && (
        <>
          <NodeNetworkStatusCard node={node} />
          {node.role === 'worker' && <WorkerIngressCard node={node} />}
          {(node.role === 'worker' || node.role === 'control-plane') && (
            <NodeNetworkPolicySettings
              key={`policy-${node.id}`}
              nodeId={node.id}
              nodeName={node.name}
            />
          )}
        </>
      )}

      {section === 'builds' && node.role === 'worker' && (
        <BuilderLimitsPanel key={node.id} node={node} />
      )}

      {section === 'scheduling' && (
        <>
          {(node.role === 'worker' || node.role === 'control-plane') && (
            <NodeSchedulingResources
              key={`resources-${node.id}`}
              nodeId={node.id}
            />
          )}
          <NodeDetailLabels labels={node.labels} />
          {(node.role === 'worker' || node.role === 'control-plane') && (
            <NodeDesiredStateStream nodeId={node.id} />
          )}
        </>
      )}
    </div>
  )
}

function ClusterTrustCard() {
  const [dialogOpen, setDialogOpen] = useState(false)
  const { fingerprint, isLoading } = useClusterCaFingerprint()

  return (
    <>
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Cluster trust
          </CardTitle>
          <CardDescription>
            The cluster CA authenticates the control plane and every worker.
            Rotation is an emergency recovery operation.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="rounded-md border bg-muted/30 p-4">
            <p className="text-sm font-medium">Active CA fingerprint</p>
            <p className="mt-2 break-all font-mono text-xs text-muted-foreground">
              {isLoading
                ? 'Loading…'
                : (fingerprint ??
                  'Initializing cluster trust… restart the control plane if this persists.')}
            </p>
          </div>
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>Emergency rotation</AlertTitle>
            <AlertDescription>
              Rotation immediately invalidates worker trust and outstanding
              enrollment tokens. Every worker must be re-enrolled. Only a full
              Admin in a browser session can proceed, and an Admin with MFA
              re-verifies it first.
            </AlertDescription>
          </Alert>
          <Button
            variant="destructive"
            onClick={() => setDialogOpen(true)}
            disabled={!fingerprint || isLoading}
          >
            Rotate cluster CA
          </Button>
        </CardContent>
      </Card>

      <RotateClusterCaDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        description="This disconnects every existing worker until it is re-enrolled against the new fingerprint. Verify the fingerprint below through a trusted operator channel before continuing."
      />
    </>
  )
}

// ── Main Page ──

export function NodesPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const {
    data,
    isLoading,
    error,
    refetch: refetchNodes,
    isFetching: nodesFetching,
  } = useQuery({
    ...adminListNodesOptions(),
    // A refused read (a role without settings:read) is not asked again.
    refetchInterval: refetchUnlessRefused(30_000),
  })
  const { data: capability } = useNodeCapability()
  const {
    data: mesh,
    isLoading: meshLoading,
    error: meshError,
    refetch: refetchMesh,
    isFetching: meshFetching,
  } = useWireguardMesh()
  const stranded = strandedPublicNodes(mesh)
  const { data: clusterNetwork } = useClusterNetwork()
  const overlayDown = (clusterNetwork?.nodes ?? []).filter(
    (node) => nodeOverlayStatus(node).tone === 'warn'
  )
  const invalidateCapability = useInvalidateNodeCapability()
  const nodes = data?.nodes ?? []
  const { data: tunnels } = useQuery(allTunnelsOptions())
  // Without local workloads the control plane serves no app traffic, so a
  // cluster whose workers all lack a way in serves nobody.
  const activeWorkers = nodes.filter(
    (node) => node.role !== 'control-plane' && node.status === 'active'
  )
  const noWayIn =
    capability?.local_workloads === false &&
    tunnels !== undefined &&
    activeWorkers.length > 0 &&
    activeWorkers.every(
      (node) => workerIngressWay(node, tunnels).kind === 'none'
    )
  const nodeCount = nodes.length
  // The list always includes this server itself; only workers count here.
  const workerCount = nodes.filter(
    (node) => node.role !== 'control-plane'
  ).length
  // "No nodes" means two very different things. With local workloads the
  // control plane runs everything itself and worker nodes are optional
  // scale-out; without them nothing can run at all and this page is the
  // onboarding step the operator must complete.
  const needsFirstNode = shouldPromptForFirstWorkerNode(capability, workerCount)
  // A cluster with no worker yet opens on adding one; otherwise it is a click.
  const [addingChosen, setAdding] = useState<boolean | null>(null)
  const adding = addingChosen ?? workerCount === 0

  // This list polls, so it is the first thing in the console to learn that a
  // worker finished `temps join` (or dropped out). Re-read the capability
  // whenever the roster changes so this page — and the banner every other
  // page renders from the same cache entry — stops contradicting it.
  useEffect(() => {
    invalidateCapability()
  }, [nodeCount, invalidateCapability])

  useEffect(() => {
    setBreadcrumbs([{ label: 'Worker nodes' }])
  }, [setBreadcrumbs])

  usePageTitle('Worker nodes')

  // A link to one of the advanced cards (search, other pages) opens the
  // section and brings the card into view. A toggle by hand wins until the
  // link changes.
  const { hash } = useLocation()
  const linkedSection = nodesAdvancedSection(hash)
  const [advancedToggle, setAdvancedToggle] = useState<{
    hash: string
    open: boolean
  } | null>(null)
  const advancedOpen =
    advancedToggle?.hash === hash ? advancedToggle.open : linkedSection !== null
  useEffect(() => {
    if (!linkedSection) return
    const frame = requestAnimationFrame(() =>
      document
        .getElementById(linkedSection)
        ?.scrollIntoView({ behavior: 'smooth', block: 'start' })
    )
    return () => cancelAnimationFrame(frame)
  }, [linkedSection, isLoading])

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <SettingsSurface>
        <div className="space-y-6">
          <PageHeader
            title="Worker nodes"
            description={
              <>Distribute container deployments across multiple servers.</>
            }
          />
          <Card>
            <CardContent>
              <ReadFailure
                resource="Worker nodes"
                error={error}
                onRetry={
                  isForbiddenRead(error) ? undefined : () => void refetchNodes()
                }
                retrying={nodesFetching}
              />
            </CardContent>
          </Card>
        </div>
      </SettingsSurface>
    )
  }

  return (
    <SettingsSurface>
      <div className="space-y-6">
        <PageHeader
          title="Worker nodes"
          description={
            <>
              Distribute container deployments across multiple servers. Worker
              nodes run the Temps agent and receive containers from the control
              plane.
            </>
          }
        />
        <Card>
          <CardContent className="space-y-6">
            {adding ? (
              <AddWorkerNode otherWays={<JoinTokenSection />} />
            ) : (
              <Button onClick={() => setAdding(true)}>
                <Plus className="mr-1 h-4 w-4" />
                Add a worker node
              </Button>
            )}

            <NodeStateStreamStatus />
            {noWayIn && (
              <Alert className="border-amber-500/30 bg-amber-500/5">
                <AlertTriangle className="h-4 w-4 text-amber-500" />
                <AlertTitle className="text-amber-700 dark:text-amber-400">
                  Visitors can&apos;t reach your apps yet
                </AlertTitle>
                <AlertDescription className="space-y-2 text-amber-700 dark:text-amber-300">
                  <p>
                    No worker takes traffic from the internet. Deployments run,
                    but their domains don&apos;t answer until a worker does,
                    through a tunnel (no public IP needed) or open ports on a
                    public IP.
                  </p>
                  <div className="flex flex-wrap gap-2">
                    <Button asChild size="sm">
                      <Link to={ingressSetupUrl()}>Set up ingress</Link>
                    </Button>
                    <Button asChild size="sm" variant="outline">
                      <Link
                        to={`/settings/nodes/${activeWorkers[0].id}#public-ingress`}
                      >
                        Open {activeWorkers[0].name}
                      </Link>
                    </Button>
                  </div>
                </AlertDescription>
              </Alert>
            )}

            {workerCount > 0 && (
              <ClusterDnsOffNotice title="Apps can't reach a database on another node">
                Cluster DNS is off, so containers can&apos;t resolve{' '}
                <code>*.temps.local</code> names. Deployments linked to a
                managed service on a different node are blocked, and a database
                cluster spanning nodes can&apos;t be reached by its apps.
              </ClusterDnsOffNotice>
            )}

            {stranded.length > 0 && (
              <Alert className="border-amber-500/30 bg-amber-500/5">
                <AlertTriangle className="h-4 w-4 text-amber-500" />
                <AlertTitle className="text-amber-700 dark:text-amber-400">
                  {stranded.map((node) => node.name).join(', ')} joined with a
                  public address
                </AlertTitle>
                <AlertDescription className="text-amber-700 dark:text-amber-300">
                  {mesh?.state === 'starting'
                    ? 'They move onto the WireGuard mesh as soon as it is up.'
                    : 'Without the private network they cannot reach the control plane or other nodes privately, so cross-node networking does not work for them. Turn it on under “Add a worker node” above.'}
                </AlertDescription>
              </Alert>
            )}

            {overlayDown.length > 0 && (
              <Alert className="border-amber-500/30 bg-amber-500/5">
                <AlertTriangle className="h-4 w-4 text-amber-500" />
                <AlertTitle className="text-amber-700 dark:text-amber-400">
                  Cross-node network unavailable on{' '}
                  {overlayDown.map((node) => node.name).join(', ')}
                </AlertTitle>
                <AlertDescription className="space-y-2 text-amber-700 dark:text-amber-300">
                  <p>
                    Their deployments keep working; containers on them cannot
                    reach containers on other nodes, and managed-service members
                    spanning hosts are not placed there.
                  </p>
                  <ul className="list-disc space-y-1 pl-4">
                    {overlayDown.map((node) => (
                      <li key={node.id} className="break-words">
                        <span className="font-medium">{node.name}:</span>{' '}
                        {nodeOverlayStatus(node).description}
                      </li>
                    ))}
                  </ul>
                </AlertDescription>
              </Alert>
            )}

            {needsFirstNode ? (
              <div className="border-t pt-6">
                <WorkerNodeRequiredAlert
                  reason={capability?.reason}
                  showSetupAction={false}
                  canManageNodes={canAddWorkerNode(capability)}
                />
              </div>
            ) : nodes.length === 0 ? (
              <div className="flex flex-col items-center justify-center py-12 text-center border-t pt-6">
                <Server className="h-12 w-12 text-muted-foreground mb-4" />
                <p className="text-sm font-medium">No worker nodes</p>
                <p className="text-sm text-muted-foreground mt-1 max-w-md">
                  All deployments run on this server. Add worker nodes to
                  distribute containers across multiple machines.
                </p>
              </div>
            ) : (
              <NodeTable
                nodes={nodes}
                mesh={mesh}
                overlay={clusterNetwork?.nodes}
              />
            )}
          </CardContent>
        </Card>

        <Collapsible
          open={advancedOpen}
          onOpenChange={(open) => setAdvancedToggle({ hash, open })}
        >
          <CollapsibleTrigger className="group inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
            <ChevronDown className="h-4 w-4 transition-transform group-data-[state=open]:rotate-180" />
            Advanced: mesh hub, cluster network, DNS and trust
          </CollapsibleTrigger>
          <CollapsibleContent className="mt-6 space-y-6">
            <div id="mesh-hub" className="scroll-mt-20">
              <MeshHubCard
                mesh={mesh}
                isLoading={meshLoading}
                error={meshError}
                onRetry={() => void refetchMesh()}
                retrying={meshFetching}
              />
            </div>
            <div id="cluster-network" className="scroll-mt-20">
              <ClusterNetworkCard />
            </div>
            <div id="cluster-dns" className="scroll-mt-20">
              <ClusterDnsCard />
            </div>
            <div id="cluster-trust" className="scroll-mt-20">
              <ClusterTrustCard />
            </div>
          </CollapsibleContent>
        </Collapsible>
      </div>
    </SettingsSurface>
  )
}
