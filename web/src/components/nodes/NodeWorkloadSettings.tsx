// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Boxes, Gauge, Loader2, Network } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ReadFailure } from '@/components/ui/read-failure'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { problemDetail } from '@/lib/api-problem'
import {
  networkPolicyKeys,
  isNodeNetworkPolicyMode,
  listNodeNetworkPolicyModes,
  networkPolicyModeUnavailableReason,
  nodeNetworkPolicyMode,
  setNodeNetworkPolicyMode,
  type NodeNetworkPolicyMode,
  type NodeNetworkPolicyModeList,
} from '@/lib/network-policy'
import {
  CONTROL_PLANE_NODE_ID,
  getNodeResources,
  getSandboxDriverMode,
  nodeResourcesQueryKey,
  parseSystemReserved,
  resourcePolicySummary,
  sandboxDriverModeQueryKey,
  sandboxDriverModeSummary,
  setNodeResources,
  setSandboxDriverMode,
  systemReservedDefaultsText,
  type NodeResourcePolicy,
  type NodeResources,
  type NodeResourcesUpdate,
  type SandboxDriverMode,
} from '@/lib/node-workload'
import { isForbiddenRead } from '@/lib/read-failure'
import { cn } from '@/lib/utils'
import { NodeStateStreamStatus } from './NodeStateStreamStatus'

interface ModeCard<T extends string> {
  value: T
  title: string
  description: string
}

const SANDBOX_MODES: ModeCard<SandboxDriverMode>[] = [
  {
    value: 'legacy',
    title: 'Legacy',
    description: 'The sandbox providers serve every call exactly as before.',
  },
  {
    value: 'shadow',
    title: 'Shadow',
    description:
      'The providers still serve; the driver facade plans every create alongside and logs where it would differ.',
  },
  {
    value: 'enforce',
    title: 'Enforce',
    description:
      'The driver facade serves. On a worker it forwards terminals, the agent runtime and disk resize to the node.',
  },
]

const RESOURCE_POLICIES: ModeCard<NodeResourcePolicy>[] = [
  {
    value: 'best_effort',
    title: 'Best effort',
    description:
      'Place by observed load (up to 90%). Requested CPU and memory are not reserved.',
  },
  {
    value: 'enforced',
    title: 'Enforced',
    description:
      "Refuse a placement whose requests don't fit the node's capacity minus the system reserve.",
  },
]

const NETWORK_MODES: ModeCard<NodeNetworkPolicyMode>[] = [
  {
    value: 'legacy',
    title: 'Legacy',
    description: "Nothing is compiled or applied: today's networking.",
  },
  {
    value: 'shadow',
    title: 'Shadow',
    description:
      'The policy is computed and logged for every workload on this node, never applied.',
  },
  {
    value: 'enforce',
    title: 'Enforce',
    description:
      'Rules and egress profiles are applied: restricted workloads reach only what a rule or link allows.',
  },
]

function ModeCards<T extends string>({
  modes,
  current,
  disabled,
  onSelect,
  label,
  unavailable,
}: {
  modes: ModeCard<T>[]
  current: T | string
  disabled: boolean
  onSelect: (value: T) => void
  label: string
  /** Modes this node cannot run; shown, but not selectable. */
  unavailable?: (value: T) => boolean
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className={cn(
        'grid grid-cols-1 gap-3',
        modes.length === 3 ? 'md:grid-cols-3' : 'md:grid-cols-2'
      )}
    >
      {modes.map((mode) => (
        <button
          key={mode.value}
          type="button"
          disabled={disabled || (unavailable?.(mode.value) ?? false)}
          aria-pressed={current === mode.value}
          onClick={() => {
            if (current !== mode.value) onSelect(mode.value)
          }}
          className={cn(
            'rounded-lg border p-4 text-left transition-colors hover:bg-muted/50 disabled:cursor-not-allowed disabled:opacity-60',
            current === mode.value && 'border-primary bg-primary/5'
          )}
        >
          <div className="font-medium">{mode.title}</div>
          <p className="mt-1 text-sm text-muted-foreground">
            {mode.description}
          </p>
        </button>
      ))}
    </div>
  )
}

function ReadOnlyNote() {
  return (
    <p className="text-sm text-muted-foreground">
      Read-only: only instance admins can change this setting.
    </p>
  )
}

function SectionSkeleton() {
  return (
    <div className="space-y-3" aria-busy="true">
      <Skeleton className="h-20 w-full" />
      <Skeleton className="h-4 w-2/3" />
    </div>
  )
}

// ── Sandbox driver mode ─────────────────────────────────────────────────────

const SANDBOX_TITLE = 'Sandbox drivers'
const SANDBOX_DESCRIPTION =
  "Choose which code path serves this node's sandboxes. Legacy is the rollback."

function SandboxDriverModeSection({
  nodeId,
  canEdit,
}: {
  nodeId: number
  canEdit: boolean
}) {
  const queryClient = useQueryClient()
  const queryKey = sandboxDriverModeQueryKey(nodeId)
  const query = useQuery({
    queryKey,
    queryFn: () => getSandboxDriverMode(nodeId),
    meta: { errorTitle: 'Could not load the sandbox driver mode' },
  })
  const update = useMutation({
    mutationFn: (mode: SandboxDriverMode) =>
      setSandboxDriverMode(nodeId, { mode }),
    meta: { errorTitle: 'Could not change the sandbox driver mode' },
    onSuccess: (setting) => {
      queryClient.setQueryData(queryKey, setting)
      toast.success(`Sandbox driver mode set to ${setting.mode}`)
    },
    onError: (error) => {
      toast.error('Could not change the sandbox driver mode', {
        description: problemDetail(error, 'The server refused the change.'),
      })
    },
  })

  return (
    <SettingsSection
      title={SANDBOX_TITLE}
      description={SANDBOX_DESCRIPTION}
      icon={Boxes}
    >
      {query.isPending ? (
        <SectionSkeleton />
      ) : query.isError ? (
        <ReadFailure
          resource="Sandbox driver mode"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : (
        <div className="space-y-4">
          <ModeCards
            label="Sandbox driver mode"
            modes={SANDBOX_MODES}
            current={query.data.mode}
            disabled={!canEdit || update.isPending}
            onSelect={(mode) => update.mutate(mode)}
          />
          <p className="text-sm text-muted-foreground" role="status">
            {update.isPending ? (
              <Loader2 className="mr-2 inline h-4 w-4 animate-spin" />
            ) : null}
            {sandboxDriverModeSummary(query.data)}
          </p>
          {canEdit ? null : <ReadOnlyNote />}
        </div>
      )}
    </SettingsSection>
  )
}

// ── Resource policy and system reserve ──────────────────────────────────────

const RESOURCES_TITLE = 'Scheduling resources'
const RESOURCES_DESCRIPTION =
  'Whether placements must fit within reserved capacity, and how much capacity Temps keeps for the OS and itself.'

function ReserveForm({
  nodeId,
  resources,
  canEdit,
  saving,
  onSave,
}: {
  nodeId: number
  resources: NodeResources
  canEdit: boolean
  saving: boolean
  onSave: (update: NodeResourcesUpdate) => void
}) {
  const [cpu, setCpu] = useState(String(resources.system_reserved.cpu_millis))
  const [memory, setMemory] = useState(
    String(resources.system_reserved.memory_mb)
  )
  const parsed = parseSystemReserved(cpu, memory)
  const changed =
    parsed.ok &&
    (parsed.value.cpu_millis !== resources.system_reserved.cpu_millis ||
      parsed.value.memory_mb !== resources.system_reserved.memory_mb)
  const cpuId = `reserved-cpu-${nodeId}`
  const memoryId = `reserved-memory-${nodeId}`

  return (
    <form
      className="space-y-3 rounded-lg border p-4"
      onSubmit={(event) => {
        event.preventDefault()
        if (parsed.ok && changed) onSave({ system_reserved: parsed.value })
      }}
    >
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">System reserve</span>
        <Badge
          variant={
            resources.system_reserved_is_default
              ? 'neutral-subtle'
              : 'warning-subtle'
          }
        >
          {resources.system_reserved_is_default ? 'Default' : 'Custom'}
        </Badge>
      </div>
      <p className="text-sm text-muted-foreground">
        {systemReservedDefaultsText(nodeId)} It is a scheduling deduction, not a
        CPU priority.
      </p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor={cpuId}>CPU (millicores)</Label>
          <Input
            id={cpuId}
            inputMode="numeric"
            value={cpu}
            disabled={!canEdit || saving}
            onChange={(event) => setCpu(event.target.value)}
            aria-invalid={!parsed.ok}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={memoryId}>Memory (MB)</Label>
          <Input
            id={memoryId}
            inputMode="numeric"
            value={memory}
            disabled={!canEdit || saving}
            onChange={(event) => setMemory(event.target.value)}
            aria-invalid={!parsed.ok}
          />
        </div>
      </div>
      {parsed.ok ? null : (
        <p className="text-sm text-destructive" role="alert">
          {parsed.error}
        </p>
      )}
      {canEdit ? (
        <div className="flex justify-end">
          <Button type="submit" size="sm" disabled={!changed || saving}>
            {saving ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : null}
            Save reserve
          </Button>
        </div>
      ) : null}
    </form>
  )
}

function NodeResourcesSection({
  nodeId,
  canEdit,
}: {
  nodeId: number
  canEdit: boolean
}) {
  const queryClient = useQueryClient()
  const queryKey = nodeResourcesQueryKey(nodeId)
  const query = useQuery({
    queryKey,
    queryFn: () => getNodeResources(nodeId),
    meta: { errorTitle: 'Could not load the scheduling resources' },
  })
  const update = useMutation({
    mutationFn: (change: NodeResourcesUpdate) =>
      setNodeResources(nodeId, change),
    meta: { errorTitle: 'Could not update the scheduling resources' },
    onSuccess: (resources) => {
      queryClient.setQueryData(queryKey, resources)
      toast.success('Scheduling resources updated')
    },
    onError: (error) => {
      toast.error('Could not update the scheduling resources', {
        description: problemDetail(error, 'The server refused the change.'),
      })
    },
  })

  return (
    <SettingsSection
      title={RESOURCES_TITLE}
      description={RESOURCES_DESCRIPTION}
      icon={Gauge}
    >
      {query.isPending ? (
        <SectionSkeleton />
      ) : query.isError ? (
        <ReadFailure
          resource="Scheduling resources"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : (
        <div className="space-y-4">
          <ModeCards
            label="Resource policy"
            modes={RESOURCE_POLICIES}
            current={query.data.resource_policy}
            disabled={!canEdit || update.isPending}
            onSelect={(policy) => update.mutate({ resource_policy: policy })}
          />
          <p className="text-sm text-muted-foreground" role="status">
            {resourcePolicySummary(query.data)}
          </p>
          <ReserveForm
            key={`${query.data.system_reserved.cpu_millis}-${query.data.system_reserved.memory_mb}`}
            nodeId={nodeId}
            resources={query.data}
            canEdit={canEdit}
            saving={update.isPending}
            onSave={(change) => update.mutate(change)}
          />
          {canEdit ? null : <ReadOnlyNote />}
        </div>
      )}
    </SettingsSection>
  )
}

// ── Network policy mode ─────────────────────────────────────────────────────

const NETWORK_TITLE = 'Network policy'
const NETWORK_DESCRIPTION =
  'Whether this node applies the network policy rules and egress profiles to the workloads it runs.'

function NetworkPolicyModeSection({
  nodeId,
  nodeName,
  canEdit,
}: {
  nodeId: number
  nodeName: string
  canEdit: boolean
}) {
  const queryClient = useQueryClient()
  const [confirmEnforce, setConfirmEnforce] = useState(false)
  const query = useQuery({
    queryKey: networkPolicyKeys.nodes,
    queryFn: listNodeNetworkPolicyModes,
    meta: { errorTitle: 'Could not load the network policy modes' },
  })
  const update = useMutation({
    mutationFn: (mode: NodeNetworkPolicyMode) =>
      setNodeNetworkPolicyMode(nodeId, mode),
    meta: { errorTitle: 'Could not change the network policy mode' },
    onSuccess: (entry) => {
      queryClient.setQueryData<NodeNetworkPolicyModeList>(
        networkPolicyKeys.nodes,
        (previous) =>
          previous
            ? {
                nodes: previous.nodes.map((node) =>
                  node.node_id === entry.node_id ? entry : node
                ),
              }
            : previous
      )
      void queryClient.invalidateQueries({ queryKey: networkPolicyKeys.all })
      toast.success(`Network policy mode set to ${entry.mode}`)
    },
    onError: (error) => {
      toast.error('Could not change the network policy mode', {
        description: problemDetail(error, 'The server refused the change.'),
      })
    },
  })
  const entry = nodeNetworkPolicyMode(query.data, nodeId)

  return (
    <SettingsSection
      title={NETWORK_TITLE}
      description={NETWORK_DESCRIPTION}
      icon={Network}
    >
      {query.isPending ? (
        <SectionSkeleton />
      ) : query.isError ? (
        <ReadFailure
          resource="Network policy mode"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : entry ? (
        <div className="space-y-4">
          <ModeCards
            label="Network policy mode"
            modes={NETWORK_MODES}
            current={entry.mode}
            disabled={!canEdit || update.isPending}
            unavailable={(mode) =>
              networkPolicyModeUnavailableReason(entry, mode) !== null
            }
            onSelect={(mode) => {
              if (mode === 'enforce') setConfirmEnforce(true)
              else update.mutate(mode)
            }}
          />
          {entry.supports_enforcement ? null : (
            <p className="text-sm text-muted-foreground">
              Shadow and Enforce are unavailable on this node.{' '}
              {networkPolicyModeUnavailableReason(entry, 'enforce')}
              {entry.mode === 'legacy'
                ? null
                : ` Its stored mode “${entry.mode}” is not applied; select Legacy to clear it.`}
            </p>
          )}
          {isNodeNetworkPolicyMode(entry.mode) ? null : (
            <p className="text-sm text-warning-strong">
              The server reports an unknown mode &ldquo;{entry.mode}&rdquo;.
            </p>
          )}
          <p className="text-sm text-muted-foreground">
            Rules are managed under{' '}
            <Link
              to="/settings/network-policy"
              className="underline underline-offset-2"
            >
              Settings → Network policy
            </Link>
            . Legacy is the rollback.
          </p>
          {canEdit ? null : <ReadOnlyNote />}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">
          This node is not in the network policy node list, so its mode cannot
          be set here.
        </p>
      )}
      <ConfirmDialog
        open={confirmEnforce}
        onOpenChange={setConfirmEnforce}
        title={`Enforce network policy on ${nodeName}?`}
        description={
          <>
            Rules and egress profiles start being applied to every workload on{' '}
            {nodeName}. Workloads with a restricted profile (sandboxes, by
            default) can no longer reach services they are not linked to, and
            unlinked services become unreachable from them. Switch back to
            Legacy to roll back.
          </>
        }
        confirmLabel="Enforce"
        destructive={false}
        onConfirm={() => {
          setConfirmEnforce(false)
          update.mutate('enforce')
        }}
      />
    </SettingsSection>
  )
}

// ── Composite ───────────────────────────────────────────────────────────────

/**
 * A node's workload-plane settings (ADR-055): sandbox drivers, scheduling
 * resources, network policy mode, and the desired-state stream it reads.
 * `nodeId` 0 is the control plane.
 */
export function NodeWorkloadSettings({
  nodeId,
  nodeName,
  hostsSandboxes = true,
}: {
  nodeId: number
  nodeName: string
  /** Edge nodes do not host sandboxes; the server refuses their mode. */
  hostsSandboxes?: boolean
}) {
  const canEdit = useIsInstanceAdmin()
  return (
    <div className="space-y-4">
      <NodeStateStreamStatus
        context={
          nodeId === CONTROL_PLANE_NODE_ID
            ? 'Desired-state stream'
            : 'Desired-state stream to workers'
        }
      />
      {hostsSandboxes ? (
        <SandboxDriverModeSection nodeId={nodeId} canEdit={canEdit} />
      ) : null}
      <NodeResourcesSection nodeId={nodeId} canEdit={canEdit} />
      <NetworkPolicyModeSection
        nodeId={nodeId}
        nodeName={nodeName}
        canEdit={canEdit}
      />
    </div>
  )
}
