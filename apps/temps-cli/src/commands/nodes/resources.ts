// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `nodes resources` (what the scheduler keeps for the OS and Temps on a node,
// and whether reservations are checked) and `nodes state-status` (the
// workload plane's desired-state stream). ADR-055 §5 and §7.

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import { adminGetNodeResources, adminSetNodeResources, nodeStateStatus } from '../../api/sdk.gen.js'
import type {
  NodeResourcePolicy,
  NodeResourcesResponse,
  NodeStateStatusResponse,
  SetNodeResourcesRequest,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, keyValue, success, error as errorOutput } from '../../ui/output.js'

// ============================================================================
// Pure helpers (unit tested)
// ============================================================================

export const NODE_RESOURCE_POLICIES: readonly NodeResourcePolicy[] = ['best_effort', 'enforced']

/**
 * A node argument for the resource commands: digits, where `0` is the
 * control plane, or `control-plane`. Unlike other node ids, 0 is valid here.
 */
export function parseResourceNodeId(value: string): number | null {
  const trimmed = value.trim().toLowerCase()
  if (trimmed === 'control-plane' || trimmed === 'cp') return 0
  if (!/^\d+$/.test(trimmed)) return null
  const id = Number(trimmed)
  return Number.isSafeInteger(id) && id <= 2_147_483_647 ? id : null
}

export interface ResourceFlags {
  policy?: string
  reserveCpu?: string
  reserveMemory?: string
}

/** What `resources set` changes, validated; reservations still unmerged. */
export interface ResourceUpdate {
  policy?: NodeResourcePolicy
  reserveCpuMillis?: number
  reserveMemoryMb?: number
}

function parseNonNegative(value: string): number | null {
  const trimmed = value.trim()
  if (!/^\d+$/.test(trimmed)) return null
  const parsed = Number(trimmed)
  return Number.isSafeInteger(parsed) && parsed <= 2_147_483_647 ? parsed : null
}

export function parseResourceFlags(flags: ResourceFlags): { update: ResourceUpdate } | { error: string } {
  const update: ResourceUpdate = {}
  if (flags.policy !== undefined) {
    const policy = flags.policy.trim().toLowerCase().replace('-', '_')
    const match = NODE_RESOURCE_POLICIES.find((p) => p === policy)
    if (!match) {
      return { error: `Unknown --policy "${flags.policy}": use one of ${NODE_RESOURCE_POLICIES.join(', ')}` }
    }
    update.policy = match
  }
  if (flags.reserveCpu !== undefined) {
    const cpu = parseNonNegative(flags.reserveCpu)
    if (cpu === null) return { error: `--reserve-cpu must be a whole number of millicores (got "${flags.reserveCpu}")` }
    update.reserveCpuMillis = cpu
  }
  if (flags.reserveMemory !== undefined) {
    const memory = parseNonNegative(flags.reserveMemory)
    if (memory === null) return { error: `--reserve-memory must be a whole number of MB (got "${flags.reserveMemory}")` }
    update.reserveMemoryMb = memory
  }
  if (update.policy === undefined && update.reserveCpuMillis === undefined && update.reserveMemoryMb === undefined) {
    return { error: 'Pass at least one of --policy, --reserve-cpu or --reserve-memory' }
  }
  return { update }
}

/**
 * The request body. The server replaces `system_reserved` as a whole, so a
 * reservation given alone is merged with the node's current other half
 * rather than refused or zeroed.
 */
export function buildNodeResourcesRequest(
  update: ResourceUpdate,
  current: NodeResourcesResponse | undefined
): SetNodeResourcesRequest {
  const body: SetNodeResourcesRequest = {}
  if (update.policy !== undefined) body.resource_policy = update.policy
  if (update.reserveCpuMillis !== undefined || update.reserveMemoryMb !== undefined) {
    body.system_reserved = {
      cpu_millis: update.reserveCpuMillis ?? current?.system_reserved.cpu_millis ?? 0,
      memory_mb: update.reserveMemoryMb ?? current?.system_reserved.memory_mb ?? 0,
    }
  }
  return body
}

/** Whether `buildNodeResourcesRequest` needs the current values first. */
export function needsCurrentReservation(update: ResourceUpdate): boolean {
  return (update.reserveCpuMillis === undefined) !== (update.reserveMemoryMb === undefined)
}

function describeNode(id: number): string {
  return id === 0 ? 'control plane' : `node ${id}`
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerNodesResourcesCommands(nodes: Command): void {
  const resources = nodes
    .command('resources')
    .description(
      'What the scheduler keeps for the OS and Temps on a node, and whether a placement that does ' +
        'not fit the remaining capacity is refused'
    )

  resources
    .command('show <node>')
    .description('Show a node\'s reservation and resource policy (node id; 0 or "control-plane" for the control plane)')
    .option('--json', 'Output in JSON format')
    .action(resourcesShowAction)

  resources
    .command('set <node>')
    .description(
      'Change a node\'s resource policy or reservation. Giving only one of --reserve-cpu and ' +
        '--reserve-memory keeps the node\'s current value for the other'
    )
    .option('--policy <policy>', 'best_effort (reservations not checked) or enforced (placements that do not fit are refused)')
    .option('--reserve-cpu <millis>', 'CPU kept for the OS and Temps, in millicores (1000 = one core)')
    .option('--reserve-memory <mb>', 'Memory kept for the OS and Temps, in MB')
    .option('--json', 'Output in JSON format')
    .action(resourcesSetAction)

  nodes
    .command('state-status')
    .description('Show the workload plane\'s desired-state stream: whether it runs here, its lag and counters')
    .option('--json', 'Output in JSON format')
    .action(stateStatusAction)
}

// ============================================================================
// Actions
// ============================================================================

function fail(message: string): void {
  errorOutput(message)
  process.exitCode = 1
}

function printResources(result: NodeResourcesResponse): void {
  newline()
  header(`${icons.globe} Resources of ${describeNode(result.node_id)}`)
  keyValue(
    'Policy',
    result.resource_policy === 'enforced'
      ? `${colors.bold('enforced')} (placements that do not fit are refused)`
      : 'best_effort (reservations are not checked)'
  )
  keyValue('Reserved CPU', `${result.system_reserved.cpu_millis}m`)
  keyValue('Reserved memory', `${result.system_reserved.memory_mb} MB`)
  if (result.system_reserved_is_default) {
    keyValue('Reservation source', colors.muted('default for the node\'s role (an estimate, not a measurement)'))
  }
  newline()
}

async function fetchResources(nodeId: number): Promise<NodeResourcesResponse> {
  const { data, error } = await adminGetNodeResources({ path: { node_id: nodeId } })
  if (error || !data) throw new Error(getErrorMessage(error))
  return data
}

async function resourcesShowAction(node: string, options: { json?: boolean }): Promise<void> {
  const nodeId = parseResourceNodeId(node)
  if (nodeId === null) return fail(`Invalid node "${node}": use a node id, or 0 / control-plane for the control plane`)
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Fetching node resources...', () => fetchResources(nodeId))
  if (options.json) {
    json(result)
    return
  }
  printResources(result)
}

async function resourcesSetAction(node: string, options: ResourceFlags & { json?: boolean }): Promise<void> {
  const nodeId = parseResourceNodeId(node)
  if (nodeId === null) return fail(`Invalid node "${node}": use a node id, or 0 / control-plane for the control plane`)
  const parsed = parseResourceFlags(options)
  if ('error' in parsed) return fail(parsed.error)
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Updating node resources...', async () => {
    const current = needsCurrentReservation(parsed.update) ? await fetchResources(nodeId) : undefined
    const { data, error } = await adminSetNodeResources({
      path: { node_id: nodeId },
      body: buildNodeResourcesRequest(parsed.update, current),
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Resources of ${describeNode(result.node_id)} updated`)
  printResources(result)
}

function printStateStatus(status: NodeStateStatusResponse): void {
  newline()
  header(`${icons.globe} Node state stream`)
  keyValue('Running', status.configured ? colors.success('yes') : colors.warning('no'))
  if (!status.configured) {
    if (status.reason) console.log(`  ${colors.muted(status.reason)}`)
    newline()
    return
  }
  keyValue('Watermark lag', `${status.watermark_lag_ms} ms`)
  keyValue('Reads', String(status.reads))
  keyValue('Full snapshots', String(status.full_snapshots))
  keyValue('Markers swept', String(status.markers_swept))
  newline()
}

async function stateStatusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const status = await withSpinner('Reading the node state stream...', async () => {
    const { data, error } = await nodeStateStatus()
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  printStateStatus(status)
}
