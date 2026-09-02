// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getTraefikDiscoveryStatus,
  listTraefikDiscoveredRoutes,
  setTraefikDiscoveredRouteEnabled,
} from '../../api/sdk.gen.js'
import type {
  TraefikDiscoveredRouteResponse,
  TraefikDiscoveryConflictResponse,
} from '../../api/types.gen.js'
import { registerTraefikDiscoveryTlsCommands } from './tls.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
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
  formatRelativeTime,
} from '../../ui/output.js'

// ============================================================================
// Display helpers (unit tested)
// ============================================================================

/** Short, stable label for a route's state, for the status column. */
export function routeStateBadgeInput(route: TraefikDiscoveredRouteResponse): string {
  if (!route.enabled) return 'inactive'
  if (route.contested_by.length > 0) return 'pending'
  return 'active'
}

/** `container:port` (plus the published host port when there is one). */
export function formatTarget(route: TraefikDiscoveredRouteResponse): string {
  const base = `${route.target_container_name}:${route.target_port}`
  return route.target_host_port === null || route.target_host_port === undefined
    ? base
    : `${base} (host :${route.target_host_port})`
}

/**
 * One line explaining why a conflicting container is not being routed.
 * Never returns an empty string: a labelled container the operator can't
 * account for is exactly the case this surface exists for.
 */
export function describeConflict(conflict: TraefikDiscoveryConflictResponse): string {
  if (conflict.reason === 'claimed_by_another_container' && conflict.winner_container_name) {
    return `${conflict.host} — '${conflict.container_name}' lost to '${conflict.winner_container_name}'`
  }
  if (conflict.reason === 'owned_by_temps_route') {
    return `${conflict.host} — '${conflict.container_name}' cannot take a Temps-managed host`
  }
  return `${conflict.host} — '${conflict.container_name}': ${conflict.detail}`
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerTraefikDiscoveryCommands(program: Command): void {
  const discovery = program
    .command('traefik-discovery')
    .description(
      'Route containers Temps did not deploy by reading their Traefik labels ' +
        '(an existing docker-compose / Coolify / Dokploy stack)'
    )

  discovery
    .command('status')
    .description(
      'Show whether Traefik label discovery is enabled on this server, which Docker network ' +
        'it watches, and what the last reconciliation found'
    )
    .option('--json', 'Output in JSON format')
    .action(statusAction)

  const routes = discovery
    .command('routes')
    .description('Inspect and suppress individual auto-discovered routes')

  routes
    .command('list')
    .alias('ls')
    .description(
      'List every route discovered from Traefik labels, including the labelled containers ' +
        'that were found but not routed, and why'
    )
    .option('-p, --page <n>', 'Page number (default: 1)')
    .option('--page-size <n>', 'Page size (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(listRoutesAction)

  routes
    .command('enable <host>')
    .description('Restore a previously suppressed discovered route')
    .option('--json', 'Output in JSON format')
    .action((host: string, options: { json?: boolean }) => setEnabledAction(host, true, options))

  routes
    .command('disable <host>')
    .description(
      'Stop routing one discovered host without touching the container labels; ' +
        'the route stays listed so you can see what was found'
    )
    .option('--json', 'Output in JSON format')
    .action((host: string, options: { json?: boolean }) => setEnabledAction(host, false, options))

  // ADR-041: TLS management subcommands.
  registerTraefikDiscoveryTlsCommands(discovery)
}

// ============================================================================
// Actions
// ============================================================================

async function statusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const status = await withSpinner('Fetching Traefik discovery status...', async () => {
    const { data, error } = await getTraefikDiscoveryStatus({ client })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(status)
    return
  }

  newline()
  header(`${icons.globe} Traefik Label Discovery`)

  if (!status.configured) {
    // Unconfigured features onboard, they never disappear: say what it would
    // do, what exactly is missing, and the concrete command that fixes it.
    keyValue('Status', colors.muted('not enabled on this server'))
    keyValue('Would watch network', status.network)
    if (status.reason) {
      keyValue('Reason', colors.warning(status.reason))
    }
    newline()
    info(
      'When enabled, Temps routes containers you did not deploy through it — an existing ' +
        'docker-compose, Coolify or Dokploy stack — by reading their traefik.enable / ' +
        'traefik.http.routers.<name>.rule labels. Nothing about those containers changes.'
    )
    newline()
    console.log(colors.bold('  Enable it with:'))
    console.log(`    ${colors.primary(status.setup.example)}`)
    if (status.setup.requires_restart) {
      console.log(
        colors.muted(
          `    (${status.setup.enable_env_var} and ${status.setup.network_env_var} are read at ` +
            'startup — restart the server after changing them)'
        )
      )
    }
    if (status.discovered_route_count > 0) {
      newline()
      warning(
        `${status.discovered_route_count} discovered route(s) are still stored from a previous ` +
          'run. Inspect them with: temps traefik-discovery routes list'
      )
    }
    newline()
    return
  }

  keyValue('Status', colors.success('running'))
  keyValue('Watched network', status.network)
  keyValue('Reconciliation interval', `${status.poll_interval_seconds}s`)
  keyValue('Discovered routes', status.discovered_route_count)
  keyValue('Serving traffic', status.enabled_route_count)

  const last = status.last_reconciliation
  if (!last) {
    newline()
    info('No reconciliation has completed yet — the first pass runs at startup.')
    newline()
    return
  }

  newline()
  header(`${icons.clock} Last reconciliation (${formatRelativeTime(last.completed_at)})`)
  keyValue('Containers scanned', last.containers_scanned)
  keyValue('Skipped (deployed by Temps)', last.skipped_temps_managed)
  keyValue('Routes upserted', last.routes_upserted)
  keyValue('Routes unchanged', last.routes_unchanged)
  keyValue('Routes removed', last.routes_removed)

  printConflicts(last.conflicts)
  newline()
}

async function listRoutesAction(options: {
  page?: string
  pageSize?: string
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  const query: { page?: number; page_size?: number } = {}
  if (options.page !== undefined) query.page = parseInt(options.page, 10)
  if (options.pageSize !== undefined) query.page_size = parseInt(options.pageSize, 10)

  const list = await withSpinner('Fetching discovered routes...', async () => {
    const { data, error } = await listTraefikDiscoveredRoutes({ client, query })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(list)
    return
  }

  newline()
  header(`${icons.globe} Traefik-Discovered Routes (${list.total})`)

  if (list.routes.length === 0) {
    if (!list.discovery_running) {
      // Distinguish "nothing found" from "the watcher isn't even running" —
      // otherwise an operator debugging alone reads an empty list as a bug.
      info('Traefik label discovery is not running on this server.')
      info('Run: temps traefik-discovery status')
    } else {
      info('No containers on the watched network carry routable Traefik labels.')
      info('Add traefik.enable=true and traefik.http.routers.<name>.rule=Host(`example.com`)')
    }
    printConflicts(list.conflicts)
    newline()
    return
  }

  const columns: TableColumn<TraefikDiscoveredRouteResponse>[] = [
    { header: 'Host', key: 'host', color: (v) => colors.bold(v) },
    { header: 'Target', accessor: (r) => formatTarget(r) },
    { header: 'Network', key: 'network' },
    { header: 'TLS', accessor: (r) => (r.tls ? 'yes' : 'no') },
    {
      header: 'State',
      accessor: (r) => routeStateBadgeInput(r),
      color: (v) => statusBadge(v),
    },
    { header: 'Last Seen', accessor: (r) => formatRelativeTime(r.last_seen_at) },
  ]

  printTable(list.routes, columns, { style: 'minimal' })

  const suppressed = list.routes.filter((r) => !r.enabled)
  if (suppressed.length > 0) {
    newline()
    console.log(colors.muted('  Suppressed (not routed):'))
    for (const route of suppressed) {
      console.log(`  ${colors.bold(route.host)}: ${colors.muted(route.inactive_reason ?? '')}`)
    }
  }

  const contested = list.routes.filter((r) => r.contested_by.length > 0)
  if (contested.length > 0) {
    newline()
    console.log(colors.muted('  Contested hosts (another container also claims these):'))
    for (const route of contested) {
      console.log(`  ${colors.bold(route.host)}: ${colors.warning(route.contested_by.join(', '))}`)
    }
  }

  printConflicts(list.conflicts)

  newline()
  console.log(
    colors.muted(
      `  Page ${list.page} · ${list.routes.length} of ${list.total} route(s)` +
        (list.discovery_running ? '' : ' · discovery watcher is NOT running')
    )
  )
  newline()
}

async function setEnabledAction(
  host: string,
  enabled: boolean,
  options: { json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const verb = enabled ? 'Enabling' : 'Disabling'
  const route = await withSpinner(`${verb} discovered route ${host}...`, async () => {
    const { data, error } = await setTraefikDiscoveredRouteEnabled({
      client,
      path: { host },
      body: { enabled },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(route)
    return
  }

  newline()
  if (route.enabled) {
    success(`${route.host} is routed again → ${formatTarget(route)}`)
  } else {
    success(`${route.host} is no longer routed (container labels were not changed)`)
    console.log(
      colors.muted(`  Restore with: temps traefik-discovery routes enable ${route.host}`)
    )
  }
  newline()
}

// ============================================================================
// Shared rendering
// ============================================================================

function printConflicts(conflicts: TraefikDiscoveryConflictResponse[]): void {
  if (conflicts.length === 0) return
  newline()
  console.log(colors.muted(`  Labelled containers that were NOT routed (${conflicts.length}):`))
  for (const conflict of conflicts) {
    console.log(`  ${icons.warning} ${describeConflict(conflict)}`)
  }
}
