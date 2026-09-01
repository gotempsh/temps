// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, keyValue, formatRelativeTime } from '../../ui/output.js'

// ============================================================================
// Hand-written request/response shapes
// ============================================================================
//
// `GET /api/cluster/dns/status` is implemented in `temps-deployments` (core,
// not a plugin) so it belongs in the generated OpenAPI client in principle.
// Generating requires `bun run spec:update` against a live server, which
// wasn't available when this command was added — see root CLAUDE.md's
// "Regenerating the OpenAPI clients". These interfaces are hand-maintained
// to mirror `crates/temps-deployments/src/handlers/nodes.rs`'s
// `ClusterDnsStatusResponse` / `NodeDnsStatusEntry` exactly. Once the spec is
// regenerated against a running server, switch this command to the
// generated types/functions and delete these.

export interface NodeDnsStatusEntry {
  node_id: number
  node_name: string
  node_status: string
  /** `null` = never reported (older agent, or a single-host node that never
   * allocates a compute_cidr and so never touches cluster DNS at all). */
  dns_resolver_running: boolean | null
  dns_resolver_tasks_alive: boolean | null
  dns_resolver_last_sync_at: string | null
  seconds_since_last_sync: number | null
  dns_resolver_consecutive_failures: number
  dns_resolver_last_error: string | null
  dns_resolver_record_count: number | null
}

export interface ClusterDnsStatusResponse {
  cluster_dns_enabled: boolean
  total_record_count: number
  nodes: NodeDnsStatusEntry[]
}

// ============================================================================
// Health classification (unit tested)
// ============================================================================

/** A resolver whose last sync is older than this is flagged degraded even
 * with zero reported failures — a stuck-but-quiet sync loop is still a
 * problem an operator needs to see. 4x the agent's 30s heartbeat interval. */
export const STALE_SYNC_THRESHOLD_SECONDS = 120

export type NodeDnsHealth = 'healthy' | 'degraded' | 'unhealthy' | 'disabled' | 'unknown'

/** Classify one node's resolver health for display. `unknown` means the node
 * has never reported (distinct from `disabled`, which means it reported and
 * is confirmed off). */
export function classifyNodeDnsHealth(entry: NodeDnsStatusEntry): NodeDnsHealth {
  if (entry.dns_resolver_running === null) return 'unknown'
  if (!entry.dns_resolver_running) return 'disabled'
  if (entry.dns_resolver_tasks_alive === false) return 'unhealthy'
  if (entry.dns_resolver_consecutive_failures > 0) return 'degraded'
  if (
    entry.seconds_since_last_sync !== null &&
    entry.seconds_since_last_sync > STALE_SYNC_THRESHOLD_SECONDS
  ) {
    return 'degraded'
  }
  return 'healthy'
}

/** Human-readable sync age for table display. */
export function formatSyncAge(entry: NodeDnsStatusEntry): string {
  if (entry.dns_resolver_last_sync_at === null) return 'never'
  return formatRelativeTime(entry.dns_resolver_last_sync_at)
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerClusterCommands(program: Command): void {
  const cluster = program.command('cluster').description('Cluster-wide multi-node operations')

  const dns = cluster.command('dns').description('Cluster DNS resolver (ADR-024) operations')

  dns
    .command('status')
    .description(
      'Show whether cluster DNS is healthy across every node — resolver status, last sync, and ' +
        'errors — without SSHing into a node to read logs'
    )
    .option('--json', 'Output in JSON format')
    .action(dnsStatusAction)
}

// ============================================================================
// Actions
// ============================================================================

async function dnsStatusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching cluster DNS status...', async () => {
    const { data, error } = await client.get<ClusterDnsStatusResponse, ProblemDetails>({
      url: 'cluster/dns/status',
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

  newline()
  header(`${icons.globe} Cluster DNS Status`)
  keyValue(
    'Cluster DNS enabled',
    result.cluster_dns_enabled ? colors.success('yes') : colors.muted('no')
  )
  keyValue('Total DNS records', result.total_record_count)

  if (result.nodes.length === 0) {
    newline()
    console.log(colors.muted('  No nodes registered'))
    newline()
    return
  }

  newline()
  const columns: TableColumn<NodeDnsStatusEntry>[] = [
    { header: 'Node', key: 'node_name', color: (v) => colors.bold(v) },
    { header: 'Node Status', accessor: (n) => n.node_status, color: (v) => statusBadge(v) },
    {
      header: 'Resolver',
      accessor: (n) => classifyNodeDnsHealth(n),
      color: (v) => statusBadge(v),
    },
    { header: 'Last Sync', accessor: (n) => formatSyncAge(n) },
    {
      header: 'Failures',
      accessor: (n) => n.dns_resolver_consecutive_failures.toString(),
      color: (v) => (parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v)),
    },
    {
      header: 'Records',
      accessor: (n) =>
        n.dns_resolver_record_count === null ? '-' : n.dns_resolver_record_count.toString(),
    },
  ]

  printTable(result.nodes, columns, { style: 'minimal' })

  const withErrors = result.nodes.filter((n) => n.dns_resolver_last_error)
  if (withErrors.length > 0) {
    newline()
    console.log(colors.muted('  Last errors:'))
    for (const node of withErrors) {
      console.log(`  ${colors.bold(node.node_name)}: ${colors.error(node.dns_resolver_last_error ?? '')}`)
    }
  }

  newline()
}
