// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Cluster-wide operations beyond `cluster dns status`: the internal DNS
 * resolvers (records, query log, traffic, a test lookup), the compute
 * network, the tokens that let nodes join, and rotating the cluster CA.
 */

import type { Command } from 'commander'
import { client } from '../../lib/api-client.js'
import {
  clusterNetworkGet,
  clusterNetworkIdChange,
  clusterNetworkUpdate,
  dnsTraffic,
  generateJoinToken,
  getJoinTokenStatus,
  listDnsQueries,
  listDnsRecords,
  listDnsResolvers,
  listEnrollmentTokens,
  mintEnrollmentToken,
  resolveDnsName,
  revokeEnrollmentToken,
  revokeJoinToken,
  rotateClusterCa,
} from '../../api/sdk.gen.js'
import type { MintEnrollmentTokenRequest, UpdateClusterNetworkRequest } from '../../api/types.gen.js'
import { call, confirmRisk, connect, output, parseIntegerFlag, type JsonOption } from '../../lib/command-kit.js'
import { info, warning, json, keyValue, newline } from '../../ui/output.js'
import { ValidationError } from '../../utils/errors.js'

/** The exact text the API requires to rotate the cluster CA. */
export const ROTATE_CONFIRMATION = 'ROTATE CLUSTER CA'

export interface PageFlags {
  page?: string
  pageSize?: string
}

export function pageQuery(flags: PageFlags): { page?: number; page_size?: number } {
  return {
    ...(flags.page !== undefined && { page: parseIntegerFlag(flags.page, '--page', 1) }),
    ...(flags.pageSize !== undefined && { page_size: parseIntegerFlag(flags.pageSize, '--page-size', 1, 100) }),
  }
}

export function mintRequest(flags: { ttl?: string; maxUses?: string; node?: string }): MintEnrollmentTokenRequest {
  return {
    ...(flags.ttl !== undefined && { ttl_secs: parseIntegerFlag(flags.ttl, '--ttl', 60) }),
    ...(flags.maxUses !== undefined && { max_uses: parseIntegerFlag(flags.maxUses, '--max-uses', 1) }),
    ...(flags.node !== undefined && { bound_node_name: flags.node }),
  }
}

export function networkRequest(flags: { computePool?: string; overlayNetwork?: string; subnetPrefix?: string }): UpdateClusterNetworkRequest {
  if (!flags.computePool) throw new ValidationError('Pass --compute-pool <cidr>, e.g. 10.200.0.0/16')
  return {
    compute_pool_cidr: flags.computePool,
    ...(flags.overlayNetwork !== undefined && { overlay_network_name: flags.overlayNetwork }),
    ...(flags.subnetPrefix !== undefined && { subnet_prefix_len: parseIntegerFlag(flags.subnetPrefix, '--subnet-prefix', 8, 30) }),
  }
}

/** Print a secret minted by this request once, to stdout, with where it goes. */
function printMintedSecret(label: string, token: string, extra: Record<string, unknown>, options: JsonOption): void {
  if (options.json) {
    json({ ...extra, token })
    return
  }
  warning(`${label} is shown only once. Store it now; it cannot be read again.`)
  newline()
  console.log(token)
  newline()
  for (const [key, value] of Object.entries(extra)) keyValue(key, value as string)
}

export function registerClusterAdminCommands(cluster: Command, dns: Command): void {
  dns.command('resolvers')
    .description("Each node's resolver: health, version, records served and last sync")
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption) => {
      await connect()
      output(await call('Could not list the DNS resolvers', () => listDnsResolvers({ client })), options, 'DNS resolvers')
    })

  dns.command('records')
    .description("The internal zone's records and who owns them, newest first")
    .option('--search <text>', 'Only records whose name contains this')
    .option('--node <id>', 'Only records served by this node')
    .option('--owner-kind <kind>', 'Only records owned by this kind (e.g. environment, service, sandbox)')
    .option('--page <n>', 'Page (default 1)')
    .option('--page-size <n>', 'Records per page (max 100)')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & PageFlags & { search?: string; node?: string; ownerKind?: string }) => {
      const query = {
        ...pageQuery(options),
        ...(options.search !== undefined && { search: options.search }),
        ...(options.node !== undefined && { node_id: parseIntegerFlag(options.node, '--node', 0) }),
        ...(options.ownerKind !== undefined && { owner_kind: options.ownerKind }),
      }
      await connect()
      output(await call('Could not list DNS records', () => listDnsRecords({ client, query })), options, 'DNS records')
    })

  dns.command('queries')
    .description('Recent queries the internal resolvers answered, newest first')
    .option('--node <id>', 'Only this node')
    .option('--outcome <outcome>', 'Only this outcome (e.g. answered, nxdomain, refused)')
    .option('--zone <zone>', 'Only this zone')
    .option('--name <name>', 'Only this queried name')
    .option('--project <id>', 'Only queries from this project ID')
    .option('--page <n>', 'Page (default 1)')
    .option('--page-size <n>', 'Queries per page (max 100)')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & PageFlags & { node?: string; outcome?: string; zone?: string; name?: string; project?: string }) => {
      const query = {
        ...pageQuery(options),
        ...(options.node !== undefined && { node_id: parseIntegerFlag(options.node, '--node', 0) }),
        ...(options.outcome !== undefined && { outcome: options.outcome }),
        ...(options.zone !== undefined && { zone: options.zone }),
        ...(options.name !== undefined && { name: options.name }),
        ...(options.project !== undefined && { project_id: parseIntegerFlag(options.project, '--project', 1) }),
      }
      await connect()
      output(await call('Could not list DNS queries', () => listDnsQueries({ client, query })), options, 'DNS queries')
    })

  dns.command('traffic')
    .description("Queries per outcome over time for one node's resolver")
    .option('--node <id>', 'Node (default: the control plane)')
    .option('--range <range>', 'Time range, e.g. 1h, 24h, 7d')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { node?: string; range?: string }) => {
      const query = {
        ...(options.node !== undefined && { node_id: parseIntegerFlag(options.node, '--node', 0) }),
        ...(options.range !== undefined && { range: options.range }),
      }
      await connect()
      output(await call('Could not read DNS traffic', () => dnsTraffic({ client, query })), options, 'DNS traffic')
    })

  dns.command('resolve <name>')
    .description("Resolve a name from one node's resolver, optionally as a project or sandbox would see it")
    .requiredOption('--node <id>', 'Node whose resolver answers')
    .option('--type <type>', 'Record type (A, AAAA, CNAME, SRV, TXT)')
    .option('--project <id>', 'Ask as this project ID')
    .option('--sandbox <id>', 'Ask as this sandbox')
    .option('--json', 'Output in JSON format')
    .action(async (name: string, options: JsonOption & { node: string; type?: string; project?: string; sandbox?: string }) => {
      const body = {
        name,
        node_id: parseIntegerFlag(options.node, '--node', 0),
        ...(options.type !== undefined && { record_type: options.type }),
        ...(options.project !== undefined && { project_id: parseIntegerFlag(options.project, '--project', 1) }),
        ...(options.sandbox !== undefined && { sandbox_id: options.sandbox }),
      }
      await connect()
      output(await call(`Could not resolve ${name}`, () => resolveDnsName({ client, body })), options, `Answer for ${name}`)
    })

  const network = cluster.command('network').description('The compute network workloads get their addresses from')
  network.command('show')
    .description('Show the compute pool, whether it can change, and each node\'s allocation')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption) => {
      await connect()
      output(await call('Could not read the cluster network', () => clusterNetworkGet({ client })), options, 'Cluster network')
    })
  network.command('set')
    .description('Change the compute pool (only while no allocation uses it)')
    .requiredOption('--compute-pool <cidr>', 'CIDR workloads get addresses from')
    .option('--overlay-network <name>', 'Docker overlay network name')
    .option('--subnet-prefix <len>', 'Per-node subnet prefix length')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { computePool?: string; overlayNetwork?: string; subnetPrefix?: string; yes?: boolean }) => {
      const body = networkRequest(options)
      await connect()
      if (!(await confirmRisk(['Changing the compute pool changes the addresses new workloads get.'], `Use ${body.compute_pool_cidr}?`, options.yes))) return
      output(await call('Could not change the cluster network', () => clusterNetworkUpdate({ client, body })), options, undefined, 'Cluster network saved')
    })
  network.command('new-network-id')
    .description('Give this cluster its own VXLAN network ID, so it cannot collide with another cluster on the same network')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { yes?: boolean }) => {
      await connect()
      if (!(await confirmRisk(['Every node re-creates its overlay with the new ID; cross-node traffic pauses meanwhile.'], 'Assign a new network ID?', options.yes))) return
      output(await call('Could not change the network ID', () => clusterNetworkIdChange({ client })), options, undefined, 'New network ID assigned')
    })

  const join = cluster.command('join-token').description('The shared token worker nodes join with')
  join.command('status')
    .description('Show whether a join token is configured')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption) => {
      await connect()
      output(await call('Could not read the join token status', () => getJoinTokenStatus({ client })), options, 'Join token')
    })
  join.command('generate')
    .description('Generate a new join token (replaces the current one; shown once)')
    .option('-y, --yes', 'Do not ask before replacing an existing token')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { yes?: boolean }) => {
      await connect()
      const status = await call('Could not read the join token status', () => getJoinTokenStatus({ client }))
      if (status.has_token && !(await confirmRisk(['A new token replaces the current one: nodes not yet joined with the old one must use the new one.'], 'Generate a new join token?', options.yes))) return
      const result = await call('Could not generate a join token', () => generateJoinToken({ client }))
      printMintedSecret('The join token', result.token, { message: result.message }, options)
    })
  join.command('revoke')
    .description('Revoke the join token; no new node can join with it')
    .option('-y, --yes', 'Do not ask for confirmation')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { yes?: boolean }) => {
      await connect()
      if (!(await confirmRisk(['Nodes already joined keep working; no new node can join with this token.'], 'Revoke the join token?', options.yes))) return
      output(await call('Could not revoke the join token', () => revokeJoinToken({ client })), options, undefined, 'Join token revoked')
    })

  const enroll = cluster.command('enrollment-tokens').description('Short-lived, single-use tokens a new node enrolls with')
  enroll.command('list')
    .alias('ls')
    .description('List valid enrollment tokens (the tokens themselves are never returned)')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption) => {
      await connect()
      output(await call('Could not list enrollment tokens', () => listEnrollmentTokens({ client })), options, 'Enrollment tokens')
    })
  enroll.command('create')
    .alias('mint')
    .description('Mint an enrollment token (shown once)')
    .option('--ttl <seconds>', 'How long it is valid')
    .option('--max-uses <n>', 'How many nodes may enroll with it')
    .option('--node <name>', 'Only a node with this name may use it')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { ttl?: string; maxUses?: string; node?: string }) => {
      const body = mintRequest(options)
      await connect()
      const result = await call('Could not mint an enrollment token', () => mintEnrollmentToken({ client, body }))
      const { token, ...rest } = result
      printMintedSecret('The enrollment token', token, rest, options)
    })
  enroll.command('revoke <id>')
    .description('Revoke an enrollment token')
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: JsonOption) => {
      const tokenId = parseIntegerFlag(id, '<id>', 1)
      await connect()
      output(await call(`Could not revoke enrollment token ${tokenId}`, () => revokeEnrollmentToken({ client, path: { id: tokenId } })), options, undefined, `Enrollment token ${tokenId} revoked`)
    })

  cluster.command('rotate-ca')
    .description('Replace a compromised cluster CA: every node re-enrolls and outstanding enrollment tokens are revoked')
    .requiredOption('--expected-fingerprint <sha256>', 'Fingerprint of the current CA, read through a trusted channel just before')
    .option('-y, --yes', `Do not ask; sends the confirmation "${ROTATE_CONFIRMATION}" for you`)
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & { expectedFingerprint: string; yes?: boolean }) => {
      await connect()
      const confirmed = await confirmRisk(
        ['Rotating the cluster CA invalidates every node certificate and enrollment token; nodes must re-enroll.'],
        'Rotate the cluster CA?',
        options.yes,
      )
      if (!confirmed) return
      const result = await call('Could not rotate the cluster CA', () =>
        rotateClusterCa({ client, body: { expected_fingerprint: options.expectedFingerprint, confirmation: ROTATE_CONFIRMATION } }),
      )
      output(result, options, undefined, 'Cluster CA rotated')
      if (!options.json) info('Re-enroll every node with a new enrollment token: temps cluster enrollment-tokens create')
    })

}
