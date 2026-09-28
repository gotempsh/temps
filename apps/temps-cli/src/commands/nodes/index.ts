// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import {
  nodeCapabilityGet,
  wireguardMeshEnable,
  wireguardMeshStatusGet,
} from '../../api/sdk.gen.js'
import type {
  NodeCapabilityResponse,
  WireguardMeshNodeConnection,
  WireguardMeshNodeStatus,
  WireguardMeshStatusResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  keyValue,
  info,
  success,
  warning,
} from '../../ui/output.js'

// ============================================================================
// Presentation (unit tested)
// ============================================================================

/** Where workloads can run, in one line, for the summary row. */
export function describeCapability(capability: NodeCapabilityResponse): string {
  if (!capability.schedulable) return 'nowhere — no schedulable target'
  const targets: string[] = []
  if (capability.local_workloads) targets.push('this host')
  if (capability.active_worker_nodes > 0) {
    targets.push(
      capability.active_worker_nodes === 1
        ? '1 worker node'
        : `${capability.active_worker_nodes} worker nodes`
    )
  }
  return targets.join(' + ')
}

/**
 * What to do about an unschedulable install, addressed to whoever is holding
 * this credential.
 *
 * A token without Settings permissions cannot mint an enrollment token, so
 * telling its owner to "configure one at /settings/nodes" sends them to a page
 * that refuses them. Say who to ask instead — the operator running this CLI
 * has no support channel to work that out from.
 */
export function capabilityRemedy(capability: NodeCapabilityResponse): string {
  if (!capability.can_manage_nodes) {
    return 'Ask an administrator to add a worker node — this credential cannot manage nodes.'
  }
  return `Join a worker node with \`temps join\`, or configure one at ${capability.setup_path}`
}

/** A node's mesh connection in words an operator can act on. */
export function describeConnection(connection: WireguardMeshNodeConnection): string {
  switch (connection) {
    case 'connected':
      return 'connected'
    case 'stale':
      return 'no handshake in 3 minutes'
    case 'never_connected':
      return 'never connected — is the mesh UDP port open?'
    case 'not_registered':
      return 'waiting for the agent to register'
    case 'waiting_for_control_plane':
      return 'waiting for the control plane'
    case 'unknown':
      return 'unknown'
    case 'mesh_off':
      return 'mesh off'
  }
}

/** The `temps join` command for a node, over the internet or a private network. */
export function joinCommand(
  mesh: Pick<WireguardMeshStatusResponse, 'join_url'>,
  overInternet: boolean
): string {
  const url = mesh.join_url ?? '<control-plane-url>'
  const address = overInternet ? '<worker-public-ip>' : '<worker-private-ip>'
  return `temps join ${url} <join-token> --private-address ${address}`
}

/**
 * Nodes that joined with a public address but cannot use the mesh: they have
 * no private path to the control plane or each other.
 */
export function strandedPublicNodes(mesh: WireguardMeshStatusResponse): WireguardMeshNodeStatus[] {
  if (mesh.state === 'ready') return []
  return mesh.nodes.filter((node) => !node.registered_on_private_network)
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerNodesCommands(program: Command): void {
  const nodes = program.command('nodes').description('Worker nodes and workload placement')

  nodes
    .command('capability')
    .description(
      'Show whether this install can run workloads at all — local workloads plus joined ' +
        'worker nodes — so a deploy that could never be scheduled is visible before it is queued'
    )
    .option('--json', 'Output in JSON format')
    .action(capabilityAction)

  const mesh = nodes
    .command('mesh')
    .description(
      'WireGuard mesh: lets nodes that only share the internet with the control plane join ' +
        'it, encrypted. Shows whether it is on and how each node is connected'
    )
    .option('--json', 'Output in JSON format')
    .action(meshStatusAction)

  mesh
    .command('enable')
    .description(
      'Turn the WireGuard mesh on. The control plane and every agent move onto it within a ' +
        'minute; it cannot be turned off again'
    )
    .option('--cidr <cidr>', 'Mesh address pool (private IPv4, clear of the compute pool)')
    .option('--port <port>', 'UDP port every node must accept from the others', (value) =>
      Number.parseInt(value, 10)
    )
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .option('--json', 'Output in JSON format')
    .action(meshEnableAction)
}

// ============================================================================
// Actions
// ============================================================================

async function capabilityAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Checking workload placement capability...', async () => {
    const { data, error } = await nodeCapabilityGet()
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.globe} Workload Placement`)
  keyValue(
    'Local workloads',
    result.local_workloads ? colors.success('enabled') : colors.muted('disabled')
  )
  keyValue('Active worker nodes', result.active_worker_nodes)
  keyValue(
    'Schedulable',
    result.schedulable ? colors.success('yes') : colors.error('no')
  )
  keyValue('Workloads run on', describeCapability(result))

  if (!result.schedulable) {
    newline()
    console.log(`  ${colors.error(result.reason ?? 'Nothing can be scheduled.')}`)
    console.log(`  ${colors.muted(capabilityRemedy(result))}`)
  }

  newline()
}

async function meshStatusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Reading WireGuard mesh state...', async () => {
    const { data, error } = await wireguardMeshStatusGet()
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }
  printMesh(result)
}

async function meshEnableAction(options: {
  cidr?: string
  port?: number
  yes?: boolean
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  if (options.port !== undefined && (!Number.isInteger(options.port) || options.port < 1 || options.port > 65535)) {
    throw new Error('--port must be a number between 1 and 65535')
  }

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message:
        'Turn on the WireGuard mesh? Every node must accept its UDP port from the others, ' +
        'cross-node traffic pauses while nodes switch, and it cannot be turned off again.',
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const result = await withSpinner('Enabling the WireGuard mesh...', async () => {
    const { data, error } = await wireguardMeshEnable({
      body: { cidr: options.cidr ?? null, listen_port: options.port ?? null },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }
  success(`WireGuard mesh enabled (${result.cidr}, UDP ${result.listen_port})`)
  printMesh(result)
}

function printMesh(mesh: WireguardMeshStatusResponse): void {
  newline()
  header(`${icons.globe} WireGuard Mesh`)
  const state =
    mesh.state === 'ready'
      ? colors.success('ready')
      : mesh.state === 'starting'
        ? colors.warning('starting')
        : colors.muted('off')
  keyValue('State', state)
  keyValue('UDP port', mesh.listen_port)
  if (mesh.cidr) keyValue('Address pool', mesh.cidr)
  if (mesh.control_plane) {
    keyValue('Control plane', `${mesh.control_plane.address} (dialed at ${mesh.control_plane.endpoint})`)
  }
  if (mesh.reason) {
    newline()
    console.log(`  ${mesh.reason}`)
  }
  if (mesh.control_plane?.endpoint_is_private) {
    warning(
      `Nodes dial the control plane at ${mesh.control_plane.endpoint}, a private address: ` +
        'nodes joining over the internet cannot reach it. Start `temps serve` with ' +
        '`--private-address <public IP>`.'
    )
  }
  if (mesh.handshake_error) {
    warning(`Handshake data unavailable: ${mesh.handshake_error}`)
  }

  if (mesh.state === 'disabled') {
    newline()
    if (mesh.can_enable) {
      console.log(`  ${colors.muted('Turn it on:')} bunx @temps-sdk/cli nodes mesh enable`)
      console.log(`  ${colors.muted('or on the control plane host:')} ${mesh.enable_command}`)
    } else if (mesh.enable_blocker) {
      console.log(`  ${colors.error(mesh.enable_blocker)}`)
    }
  }

  const stranded = strandedPublicNodes(mesh)
  if (stranded.length > 0) {
    newline()
    warning(
      `${stranded.map((node) => node.name).join(', ')} joined with a public address and cannot ` +
        'reach other nodes until the mesh is ready.'
    )
  }

  if (mesh.nodes.length > 0) {
    newline()
    printTable(mesh.nodes, [
      { header: 'Node', key: 'name' },
      {
        header: 'Joined over',
        accessor: (node) => (node.registered_on_private_network ? 'private network' : 'internet'),
      },
      { header: 'Registered', key: 'registered_address' },
      { header: 'Mesh address', accessor: (node) => node.mesh_address ?? '—' },
      { header: 'Connection', accessor: (node) => describeConnection(node.connection) },
    ])
  }

  newline()
  console.log(`  ${colors.muted('Join a node over the internet:')}`)
  console.log(`    ${joinCommand(mesh, true)}`)
  if (!mesh.join_url) {
    console.log(
      `  ${colors.muted('Set the external URL in Settings so this shows the address nodes reach.')}`
    )
  }
  newline()
}
