// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import {
  nodeCapabilityGet,
  nodePairingCancel,
  nodePairingCreate,
  nodePairingList,
  wireguardMeshEnable,
  wireguardMeshHubSet,
  wireguardMeshStatusGet,
} from '../../api/sdk.gen.js'
import type {
  NodeCapabilityResponse,
  NodePairingResponse,
  WireguardMeshCheckStatus,
  WireguardMeshHubTarget,
  WireguardMeshLink,
  WireguardMeshNodeConnection,
  WireguardMeshNodeStatus,
  WireguardMeshStatusResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { registerNodesSshCommands } from './ssh.js'
import { parseId, parsePortOption, validPort } from './options.js'
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

/** A pairing's progress in words an operator can act on. */
export function describePairing(pairing: NodePairingResponse): string {
  switch (pairing.status) {
    case 'waiting':
      if (pairing.last_rejection) return `refused: ${pairing.last_rejection}`
      return pairing.last_error
        ? `waiting for the node: ${pairing.last_error}`
        : 'waiting for the node to run the pairing command'
    case 'key_received':
      return 'the node answered; it is registering over the mesh'
    case 'completed':
      return pairing.node_id ? `joined as node ${pairing.node_id}` : 'joined'
    case 'expired':
      return 'expired; create a new pairing'
    case 'cancelled':
      return 'cancelled'
    default:
      return pairing.status
  }
}

/** Pairings still in progress. */
export function pendingPairings(pairings: NodePairingResponse[]): NodePairingResponse[] {
  return pairings.filter(
    (pairing) => pairing.status === 'waiting' || pairing.status === 'key_received'
  )
}

/** One `nodes mesh doctor` finding: a state, and what fixes it. */
export interface MeshDoctorFinding {
  /** "cluster", a node name, or "pairing <name>". */
  scope: string
  label: string
  status: WireguardMeshCheckStatus
  detail: string
  fix?: string | null
}

/**
 * Everything the control plane can tell about the mesh (ADR 048 D9): its own
 * end, each node's link and each pairing in progress. A node's own view of
 * its end is `temps doctor mesh` on that node.
 */
export function meshDoctorFindings(
  mesh: WireguardMeshStatusResponse,
  pairings: NodePairingResponse[]
): MeshDoctorFinding[] {
  const findings: MeshDoctorFinding[] = []
  const cluster = (
    label: string,
    status: WireguardMeshCheckStatus,
    detail: string,
    fix?: string
  ) => findings.push({ scope: 'cluster', label, status, detail, fix })

  if (mesh.state === 'disabled') {
    cluster(
      'Mesh',
      'info',
      'off: nodes must reach this control plane and each other on a private network',
      mesh.can_enable
        ? 'To add nodes over the internet: bunx @temps-sdk/cli nodes mesh enable'
        : (mesh.enable_blocker ?? undefined)
    )
    return findings
  }
  if (mesh.state === 'starting') {
    cluster(
      'Mesh',
      'fail',
      mesh.reason ?? "on, but the control plane has not brought its end up",
      "Check the `temps serve` logs for WireGuard errors, or run `temps doctor mesh` on the control plane."
    )
  } else {
    const endpoint = mesh.control_plane?.endpoint
    cluster(
      'Control plane',
      'pass',
      endpoint
        ? `up at ${mesh.control_plane?.address}, dialed at ${endpoint}`
        : `up at ${mesh.control_plane?.address}; it dials the nodes (they cannot dial it)`
    )
    if (mesh.control_plane?.endpoint_is_private) {
      cluster(
        'Control plane endpoint',
        'warn',
        `${endpoint} is a private address: nodes on the internet cannot dial it`,
        'Pair such nodes from here instead (bunx @temps-sdk/cli nodes pair create --address <ip>), or start `temps serve` with --private-address <public ip>.'
      )
    }
  }
  if (mesh.handshake_error) {
    cluster(
      'Handshakes',
      'warn',
      `the control plane could not read them: ${mesh.handshake_error}`,
      'Check that `temps serve` runs as root or with CAP_NET_ADMIN.'
    )
  }
  if (mesh.state === 'ready') {
    const unreachable = mesh.links.filter((link) => link.state === 'unreachable')
    if (mesh.hub) {
      cluster('Hub', 'info', `${mesh.hub.name} relays between members that cannot reach each other`)
    } else if (unreachable.length > 0) {
      cluster(
        'Hub',
        'fail',
        `none set, and ${unreachable.length} pair(s) cannot reach each other`,
        'bunx @temps-sdk/cli nodes mesh hub set <member>: a member both sides reach (see each node\'s Links check)'
      )
    }
  }
  for (const node of mesh.nodes) {
    for (const check of node.checks) {
      findings.push({ scope: node.name, ...check })
    }
  }
  for (const pairing of pendingPairings(pairings)) {
    const scope = `pairing ${pairing.name}`
    if (pairing.status === 'key_received') {
      findings.push({
        scope,
        label: 'Pairing',
        status: 'info',
        detail: 'the node answered; it is registering over the mesh',
      })
    } else if (pairing.last_rejection) {
      findings.push({
        scope,
        label: 'Pairing',
        status: 'fail',
        detail: pairing.last_rejection,
      })
    } else {
      findings.push({
        scope,
        label: 'Pairing',
        status: pairing.last_error ? 'warn' : 'info',
        detail: pairing.last_error ?? 'waiting for the node to run the pairing command',
      })
    }
  }
  return findings
}

/** How a pair of mesh members reaches each other. */
export function describeLink(link: WireguardMeshLink): string {
  switch (link.state) {
    case 'direct':
      return 'direct'
    case 'via_hub':
      return 'through the hub'
    case 'connecting':
      return 'connecting'
    case 'unreachable':
      return 'cannot reach each other'
  }
}

/** The pairs worth showing: every one that is not simply direct. */
export function linksNeedingAttention(links: WireguardMeshLink[]): WireguardMeshLink[] {
  return links.filter((link) => link.state !== 'direct')
}

/**
 * The hub `nodes mesh hub set <member>` names: `control-plane`, or a node by
 * name or id. Throws with the members to choose from when it names none.
 */
export function hubTargetFor(
  member: string,
  mesh: Pick<WireguardMeshStatusResponse, 'nodes'>
): WireguardMeshHubTarget {
  if (member === 'control-plane') return { kind: 'control_plane' }
  const node =
    mesh.nodes.find((candidate) => candidate.name === member) ??
    mesh.nodes.find((candidate) => String(candidate.node_id) === member)
  if (!node) {
    const choices = ['control-plane', ...mesh.nodes.map((candidate) => candidate.name)]
    throw new Error(`no mesh member named ${member}; choose one of: ${choices.join(', ')}`)
  }
  return { kind: 'node', node_id: node.node_id }
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
    .option('--port <port>', 'UDP port every node must accept from the others', parsePortOption)
    .option(
      '--node-api-port <port>',
      'TCP port nodes reach this control plane on over the mesh (default: the mesh port)',
      parsePortOption
    )
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .option('--json', 'Output in JSON format')
    .action(meshEnableAction)

  const hub = mesh
    .command('hub')
    .description(
      'Mesh hub: a member that relays between members that cannot reach each other (two ' +
        'nodes behind NAT). Shows the hub and every pair it carries'
    )
    .option('--json', 'Output in JSON format')
    .action(meshHubShowAction)

  hub
    .command('set <member>')
    .description(
      'Make <member> the hub: control-plane, or a node name. Pairs that never connect move ' +
        'onto it within a few minutes. The hub can read the traffic it relays: pick your own machine'
    )
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .option('--json', 'Output in JSON format')
    .action(meshHubSetAction)

  hub
    .command('unset')
    .description('Remove the hub: relayed pairs go back to trying the direct path')
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .option('--json', 'Output in JSON format')
    .action(meshHubUnsetAction)

  mesh
    .command('doctor')
    .description(
      'Check the mesh from the control plane: its end, every node link and every pairing in ' +
        'progress, each failure with what fixes it. Exits 1 when a check fails. For a node\'s ' +
        'own end, run `temps doctor mesh` on it'
    )
    .option('--json', 'Output in JSON format')
    .action(meshDoctorAction)

  const pair = nodes
    .command('pair')
    .description(
      'Pair nodes this control plane dials: for a control plane nodes cannot reach (a ' +
        'laptop, a server behind NAT). Lists recent pairings and their progress'
    )
    .option('--json', 'Output in JSON format')
    .action(pairListAction)

  pair
    .command('create')
    .description(
      'Start pairing the node at --address. Prints the one command to run on it; the ' +
        'control plane then dials it on the mesh port until it answers (30 minutes)'
    )
    .requiredOption('--address <ip[:port]>', "The node's public IP (and mesh port, if not the default)")
    .option('--name <name>', 'Name the node registers under (default: worker-<random>)')
    .option('--json', 'Output in JSON format (includes the command, which holds a secret)')
    .action(pairCreateAction)

  pair
    .command('cancel <id>')
    .description('Cancel a pending pairing: its command stops working and its address is released')
    .option('-y, --yes', 'Skip the confirmation prompt (for automation)')
    .option('--json', 'Output in JSON format')
    .action(pairCancelAction)

  registerNodesSshCommands(nodes)
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

async function meshDoctorAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const [mesh, pairings] = await withSpinner('Checking the WireGuard mesh...', async () => {
    const [status, pairingList] = await Promise.all([
      wireguardMeshStatusGet(),
      nodePairingList(),
    ])
    if (status.error || !status.data) {
      throw new Error(getErrorMessage(status.error))
    }
    if (pairingList.error || !pairingList.data) {
      throw new Error(getErrorMessage(pairingList.error))
    }
    return [status.data, pairingList.data.pairings] as const
  })

  const findings = meshDoctorFindings(mesh, pairings)
  const failed = findings.filter((finding) => finding.status === 'fail').length
  if (options.json) {
    json(findings)
  } else {
    newline()
    header(`${icons.globe} Mesh doctor`)
    let scope: string | undefined
    for (const finding of findings) {
      if (finding.scope !== scope) {
        scope = finding.scope
        newline()
        console.log(`  ${colors.bold(scope)}`)
      }
      const mark =
        finding.status === 'pass'
          ? colors.success('PASS')
          : finding.status === 'fail'
            ? colors.error('FAIL')
            : finding.status === 'warn'
              ? colors.warning('WARN')
              : colors.muted('INFO')
      console.log(`    ${mark} ${finding.label}: ${finding.detail}`)
      if (finding.fix) console.log(`         ${colors.muted('fix:')} ${finding.fix}`)
    }
    newline()
    if (failed > 0) {
      warning(`${failed} check(s) failed`)
    } else {
      success('No failing checks')
    }
    newline()
  }
  if (failed > 0) process.exitCode = 1
}

async function readMesh(): Promise<WireguardMeshStatusResponse> {
  const { data, error } = await wireguardMeshStatusGet()
  if (error || !data) {
    throw new Error(getErrorMessage(error))
  }
  return data
}

async function meshHubShowAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const mesh = await withSpinner('Reading the WireGuard mesh...', readMesh)
  if (options.json) {
    json({ hub: mesh.hub ?? null, links: mesh.links })
    return
  }
  newline()
  header(`${icons.globe} Mesh hub`)
  printHubAndLinks(mesh)
  newline()
}

async function setHub(
  target: WireguardMeshHubTarget,
  options: { json?: boolean }
): Promise<void> {
  const mesh = await withSpinner('Setting the mesh hub...', async () => {
    const { data, error } = await wireguardMeshHubSet({ body: { hub: target } })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })
  if (options.json) {
    json({ hub: mesh.hub ?? null, links: mesh.links })
    return
  }
  if (mesh.hub) {
    success(`${mesh.hub.name} is the mesh hub`)
    console.log(
      `  ${colors.muted('Pairs that never connect move onto it within a few minutes:')} bunx @temps-sdk/cli nodes mesh hub`
    )
  } else {
    success('The mesh has no hub')
  }
}

async function meshHubSetAction(
  member: string,
  options: { yes?: boolean; json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const mesh = await withSpinner('Reading the WireGuard mesh...', readMesh)
  const target = hubTargetFor(member, mesh)
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message:
        `Make ${member} the mesh hub? It relays, and can read, the traffic between members ` +
        'that cannot reach each other.',
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await setHub(target, options)
}

async function meshHubUnsetAction(options: { yes?: boolean; json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: 'Remove the mesh hub? Members that only reach each other through it lose that link.',
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await setHub({ kind: 'none' }, options)
}

function printHubAndLinks(mesh: WireguardMeshStatusResponse): void {
  keyValue('Hub', mesh.hub ? mesh.hub.name : colors.muted('none'))
  const shown = linksNeedingAttention(mesh.links)
  if (mesh.links.length > 0 && shown.length === 0) {
    keyValue('Links', colors.success(`all ${mesh.links.length} pairs connect directly`))
    return
  }
  if (shown.length === 0) return
  newline()
  printTable(shown, [
    { header: 'Between', accessor: (link) => `${link.a} ↔ ${link.b}` },
    { header: 'Link', accessor: (link) => describeLink(link) },
  ])
  for (const link of shown.filter((candidate) => candidate.state === 'unreachable')) {
    if (link.detail) console.log(`  ${colors.muted(`${link.a} ↔ ${link.b}:`)} ${link.detail}`)
  }
  if (!mesh.hub && shown.some((link) => link.state === 'unreachable')) {
    console.log(`  ${colors.muted('Relay them:')} bunx @temps-sdk/cli nodes mesh hub set <member>`)
  }
}

async function meshEnableAction(options: {
  cidr?: string
  port?: number
  nodeApiPort?: number
  yes?: boolean
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!validPort(options.port)) {
    throw new Error('--port must be a number between 1 and 65535')
  }
  if (!validPort(options.nodeApiPort)) {
    throw new Error('--node-api-port must be a number between 1 and 65535')
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
      body: {
        cidr: options.cidr ?? null,
        listen_port: options.port ?? null,
        node_api_port: options.nodeApiPort ?? null,
      },
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

async function pairListAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Reading node pairings...', async () => {
    const { data, error } = await nodePairingList()
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data.pairings
  })

  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.globe} Node Pairings`)
  if (result.length === 0) {
    console.log(
      `  ${colors.muted('None yet. Pair a node:')} bunx @temps-sdk/cli nodes pair create --address <node-public-ip>`
    )
    newline()
    return
  }
  printTable(result, [
    { header: 'ID', key: 'id' },
    { header: 'Node', key: 'name' },
    { header: 'Address', key: 'node_endpoint' },
    { header: 'Mesh address', key: 'mesh_address' },
    { header: 'Progress', accessor: (pairing) => describePairing(pairing) },
  ])
  newline()
}

async function pairCreateAction(options: {
  address: string
  name?: string
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Creating the pairing...', async () => {
    const { data, error } = await nodePairingCreate({
      body: { address: options.address, name: options.name ?? null },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    // stdout stays pure JSON; the warning goes to stderr.
    console.error(
      'Warning: this output contains a one-time secret (join_command). Do not log or share it.'
    )
    json(result)
    return
  }
  success(`Pairing ${result.pairing.name} (${result.pairing.node_endpoint}) created`)
  newline()
  console.log(`  ${colors.muted('On the node, as root (the command holds a secret; it is shown once):')}`)
  console.log(`    ${result.join_command}`)
  console.log(`  ${colors.muted('Then:')} temps agent service install`)
  newline()
  console.log(
    `  ${colors.muted('The control plane dials')} ${result.pairing.node_endpoint} ` +
      `${colors.muted('over UDP until the node answers; it must accept that port from this control plane.')}`
  )
  console.log(`  ${colors.muted('Progress:')} bunx @temps-sdk/cli nodes pair`)
  newline()
}

async function pairCancelAction(
  id: string,
  options: { yes?: boolean; json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const pairingId = parseId(id)
  if (pairingId === null) {
    throw new Error('the pairing id must be a number (see `bunx @temps-sdk/cli nodes pair`)')
  }
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Cancel pairing ${pairingId}? Its command stops working.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await withSpinner('Cancelling the pairing...', async () => {
    const { error } = await nodePairingCancel({ path: { pairing_id: pairingId } })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })
  if (options.json) {
    json({ id: pairingId, cancelled: true })
    return
  }
  success(`Pairing ${pairingId} cancelled`)
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
    keyValue(
      'Control plane',
      mesh.control_plane.endpoint
        ? `${mesh.control_plane.address} (dialed at ${mesh.control_plane.endpoint})`
        : `${mesh.control_plane.address} (no public endpoint: it dials nodes that have one)`
    )
  }
  if (mesh.reason) {
    newline()
    console.log(`  ${mesh.reason}`)
  }
  if (mesh.control_plane?.endpoint_is_private) {
    warning(
      `Nodes dial the control plane at ${mesh.control_plane.endpoint}, a private address: ` +
        'nodes joining over the internet cannot reach it. Pair them from here instead ' +
        '(bunx @temps-sdk/cli nodes pair create --address <node-public-ip>), or start ' +
        '`temps serve` with `--private-address <public IP>`.'
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

  if (mesh.state === 'ready') {
    printHubAndLinks(mesh)
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
  console.log(`  ${colors.muted('Join a node over the internet (it must reach this control plane):')}`)
  console.log(`    ${joinCommand(mesh, true)}`)
  console.log(`  ${colors.muted('Or pair a node this control plane can reach:')}`)
  console.log('    bunx @temps-sdk/cli nodes pair create --address <node-public-ip>')
  if (!mesh.join_url) {
    console.log(
      `  ${colors.muted('Set the external URL in Settings so this shows the address nodes reach.')}`
    )
  }
  newline()
}
