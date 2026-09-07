// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import { getPostgresWalHealth } from '../../api/sdk.gen.js'
import type { PostgresWalHealth, WalWarning } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import {
  newline,
  header,
  icons,
  json as jsonOut,
  colors,
  error as errorOutput,
  keyValue,
  formatRelativeTime,
} from '../../ui/output.js'

interface WalHealthOptions {
  id: string
  json?: boolean
}

function formatBytes(n: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function describeWarning(warning: WalWarning): string {
  switch (warning.kind) {
    case 'wal_bloat':
      return `WAL directory is ${warning.ratio.toFixed(1)}x max_wal_size (${formatBytes(warning.pg_wal_bytes)} vs ${formatBytes(warning.max_wal_size_bytes)})`
    case 'stale_slot':
      return `Replication slot "${warning.slot_name}" is retaining ${formatBytes(warning.retained_bytes)} of WAL${warning.active ? '' : ' (inactive slot)'}`
    case 'archive_backlog':
      return `${warning.ready_count} WAL segment(s) waiting to be archived`
    case 'archive_mode_without_command':
      return 'archive_mode is on but archive_command is empty or /bin/true — WAL is never actually shipped'
    case 'wal_not_recycled':
      return `Oldest WAL segment is ${Math.round(warning.oldest_age_secs / 3600)}h old and has not been recycled`
    default:
      return JSON.stringify(warning)
  }
}

function severityOf(warning: WalWarning): 'warning' | 'critical' {
  switch (warning.kind) {
    case 'stale_slot':
      return 'critical'
    case 'wal_bloat':
      return warning.ratio >= 10.0 ? 'critical' : 'warning'
    default:
      return 'warning'
  }
}

async function walHealthAction(options: WalHealthOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const serviceId = parseInt(options.id, 10)
  if (!Number.isFinite(serviceId)) {
    errorOutput(`Invalid service id: ${options.id}`)
    process.exit(1)
  }

  const health = await withSpinner('Probing WAL / archive_command health...', async () => {
    const { data, error } = await getPostgresWalHealth({ path: { id: serviceId } })
    if (error) throw new Error(getErrorMessage(error))
    return data as PostgresWalHealth
  })

  if (options.json) {
    jsonOut(health)
    return
  }

  newline()
  header(`${icons.info} Postgres WAL health`)
  keyValue('Probed', formatRelativeTime(health.probed_at))
  keyValue('archive_mode', health.archive_mode)
  keyValue('archive_command', health.archive_command ?? colors.muted('(not set)'))
  keyValue('Archive backlog', `${health.archive_backlog} segment(s) waiting`)
  keyValue(
    'Archiver failures',
    health.archiver_failed_count
      ? colors.error(
          `${health.archiver_failed_count} (last: ${
            health.archiver_last_failed_at
              ? formatRelativeTime(health.archiver_last_failed_at)
              : 'unknown'
          })`,
        )
      : colors.success('0'),
  )
  keyValue('pg_wal size', formatBytes(health.pg_wal_bytes))
  keyValue('max_wal_size', formatBytes(health.max_wal_size_bytes))
  keyValue('Oldest WAL age', `${Math.round(health.oldest_wal_age_secs / 60)}m`)
  if (health.stale_slots.length > 0) {
    newline()
    header('Replication slots')
    for (const slot of health.stale_slots) {
      keyValue(
        slot.slot_name,
        `${formatBytes(slot.retained_bytes)} retained${slot.active ? '' : ' (inactive)'}`,
      )
    }
  }

  newline()
  if (health.warnings.length === 0) {
    header(`${icons.success} No warnings`)
  } else {
    header(`${icons.warning} Warnings (${health.warnings.length})`)
    for (const warning of health.warnings) {
      const line = describeWarning(warning)
      console.log(severityOf(warning) === 'critical' ? colors.error(`  ✗ ${line}`) : colors.warning(`  ! ${line}`))
    }
  }
}

// ---- Registration --------------------------------------------------------

export function registerWalHealthCommands(services: Command): void {
  services
    .command('wal-health')
    .description(
      'Probe a PostgreSQL service\'s WAL / archive_command health right now (archiver failures, backlog, stale replication slots) — diagnoses "Cloud backup mirror unavailable ... check that PostgreSQL\'s archive_command is succeeding" warnings',
    )
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(walHealthAction)
}
