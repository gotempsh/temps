// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  bulkUpdateProjectAlarms,
  bulkUpdateSystemAlarms,
  getProjectBySlug,
  getProjectAlarmsSummary,
  getSystemAlarmsSummary,
  listProjectAlarms,
  listSystemAlarms,
} from '../../api/sdk.gen.js'
import type {
  AlarmListResponse,
  AlarmResponse,
  AlarmSummaryResponse,
  BulkAlarmActionRequest,
  BulkAlarmFilter,
  BulkAlarmRequest,
  BulkAlarmResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, formatDate } from '../../ui/output.js'

/** Server-side cap on alarms changed by one bulk request. */
export const MAX_BULK_ALARM_IDS = 1000

/**
 * Upper bound on follow-up requests for `--all`. Each request clears up to
 * 1000 alarms, so this covers 100k matching alarms before stopping instead
 * of looping forever if alarms keep firing faster than they are cleared.
 */
const MAX_BULK_ROUNDS = 100

interface ScopeOptions {
  project?: string
  projectId?: string
  system?: boolean
}

interface FilterOptions {
  status?: string
  severity?: string
  type?: string
  environmentId?: string
  deploymentId?: string
}

interface ListOptions extends ScopeOptions, FilterOptions {
  page?: string
  pageSize?: string
  json?: boolean
}

interface SummaryOptions extends ScopeOptions {
  json?: boolean
}

interface BulkOptions extends ScopeOptions, FilterOptions {
  all?: boolean
  json?: boolean
}

/** Where alarms live: one project, or host-wide system alarms. */
export type AlarmScope = { kind: 'project'; projectId: number } | { kind: 'system' }

function addScopeOptions(command: Command): Command {
  return command
    .option('-p, --project <slug>', 'Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT)')
    .option('--project-id <id>', 'Project ID (instead of --project)')
    .option('--system', 'Target host-wide system alarms (disk space, worker nodes) instead of a project')
}

function addFilterOptions(command: Command): Command {
  return command
    .option('--status <status>', 'Filter by status (firing, acknowledged, resolved)')
    .option('--severity <severity>', 'Filter by severity (info, warning, critical)')
    .option('--type <type>', 'Filter by alarm type (e.g. container_crash)')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--deployment-id <id>', 'Filter by deployment ID')
}

export function registerAlarmsCommands(program: Command): void {
  const alarms = program
    .command('alarms')
    .alias('alarm')
    .description('List, acknowledge, and resolve alarms (container crashes, uptime, metrics, databases)')

  addFilterOptions(addScopeOptions(
    alarms
      .command('list')
      .alias('ls')
      .description('List alarms, newest first'),
  ))
    .option('--page <n>', 'Page number (default: 1)')
    .option('--page-size <n>', 'Items per page (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(listAlarmsAction)

  addScopeOptions(
    alarms
      .command('summary')
      .description('Show active alarm counts by status, severity, and type'),
  )
    .option('--json', 'Output in JSON format')
    .action(summaryAction)

  for (const [name, alias, action, verb] of [
    ['ack', 'acknowledge', 'acknowledge', 'Acknowledge'],
    ['resolve', undefined, 'resolve', 'Resolve'],
  ] as const) {
    const command = alarms.command(`${name} [alarmIds...]`)
    if (alias) command.alias(alias)
    addFilterOptions(addScopeOptions(
      command.description(
        `${verb} alarms by ID, or every alarm matching the filters with --all`,
      ),
    ))
      .option('--all', 'Target every alarm matching the filters instead of explicit IDs')
      .option('--json', 'Output in JSON format')
      .addHelpText('after', `
Examples:
  $ bunx @temps-sdk/cli alarms ${name} 12 13 14 -p my-app
  $ bunx @temps-sdk/cli alarms ${name} --all --type container_crash -p my-app
  $ bunx @temps-sdk/cli alarms ${name} --all --system`)
      .action((ids: string[], options: BulkOptions) => bulkAction(action, ids, options))
  }
}

/** Parse positional alarm IDs, rejecting anything that isn't a positive integer. */
export function parseAlarmIds(raw: string[]): number[] {
  const ids = raw.map((value) => {
    const id = Number(value)
    if (!Number.isInteger(id) || id <= 0) {
      throw new Error(`Invalid alarm ID "${value}": expected a positive integer`)
    }
    return id
  })
  return [...new Set(ids)]
}

function parseOptionalId(value: string | undefined, flag: string): number | undefined {
  if (value === undefined) return undefined
  const id = Number(value)
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`Invalid ${flag} "${value}": expected a positive integer`)
  }
  return id
}

/** Build the bulk `filter` body from CLI flags. */
export function buildBulkFilter(options: FilterOptions): BulkAlarmFilter {
  const filter: BulkAlarmFilter = {}
  if (options.status) filter.status = options.status
  if (options.severity) filter.severity = options.severity
  if (options.type) filter.alarm_type = options.type
  const environmentId = parseOptionalId(options.environmentId, '--environment-id')
  if (environmentId !== undefined) filter.environment_id = environmentId
  const deploymentId = parseOptionalId(options.deploymentId, '--deployment-id')
  if (deploymentId !== undefined) filter.deployment_id = deploymentId
  return filter
}

/**
 * Validate the ID/`--all` combination and build the request body. Explicit IDs
 * and `--all` are mutually exclusive; filters only make sense with `--all`.
 */
export function buildBulkRequest(
  action: BulkAlarmActionRequest,
  rawIds: string[],
  options: BulkOptions,
): BulkAlarmRequest {
  const hasFilters = Boolean(
    options.status || options.severity || options.type || options.environmentId || options.deploymentId,
  )
  if (rawIds.length > 0 && options.all) {
    throw new Error('Pass alarm IDs or --all, not both')
  }
  if (rawIds.length > 0) {
    if (hasFilters) {
      throw new Error('Filters (--status, --severity, --type, ...) only apply with --all')
    }
    const alarm_ids = parseAlarmIds(rawIds)
    if (alarm_ids.length > MAX_BULK_ALARM_IDS) {
      throw new Error(`At most ${MAX_BULK_ALARM_IDS} alarm IDs per request (got ${alarm_ids.length}); use --all with filters instead`)
    }
    return { action, alarm_ids }
  }
  if (!options.all) {
    throw new Error('Pass one or more alarm IDs, or --all to target every alarm matching the filters')
  }
  return { action, filter: buildBulkFilter(options) }
}

async function resolveScope(options: ScopeOptions): Promise<AlarmScope> {
  if (options.system) {
    if (options.project || options.projectId) {
      throw new Error('--system cannot be combined with --project or --project-id')
    }
    return { kind: 'system' }
  }
  const projectId = parseOptionalId(options.projectId, '--project-id')
  if (projectId !== undefined) {
    return { kind: 'project', projectId }
  }
  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { kind: 'project', projectId: data.id }
}

function scopeLabel(scope: AlarmScope): string {
  return scope.kind === 'system' ? 'system' : `project ${scope.projectId}`
}

function severityColor(severity: string): string {
  switch (severity) {
    case 'critical':
      return colors.error(severity)
    case 'warning':
      return colors.warning(severity)
    default:
      return colors.muted(severity)
  }
}

function statusColor(status: string): string {
  switch (status) {
    case 'firing':
      return colors.error(status)
    case 'acknowledged':
      return colors.warning(status)
    default:
      return colors.muted(status)
  }
}

async function listAlarmsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = await resolveScope(options)

  const query = {
    ...(options.status && { status: options.status }),
    ...(options.severity && { severity: options.severity }),
    ...(options.type && { alarm_type: options.type }),
    ...(options.environmentId && { environment_id: parseOptionalId(options.environmentId, '--environment-id') }),
    ...(options.deploymentId && { deployment_id: parseOptionalId(options.deploymentId, '--deployment-id') }),
    ...(options.page && { page: parseOptionalId(options.page, '--page') }),
    ...(options.pageSize && { page_size: parseOptionalId(options.pageSize, '--page-size') }),
  }

  const list = await withSpinner('Fetching alarms...', async (): Promise<AlarmListResponse> => {
    const { data, error } = scope.kind === 'system'
      ? await listSystemAlarms({ client, query })
      : await listProjectAlarms({ client, path: { project_id: scope.projectId }, query })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to list alarms')
    }
    return data
  })

  if (options.json) {
    json(list)
    return
  }

  newline()
  header(`${icons.warning} Alarms for ${scopeLabel(scope)} (${list.total})`)
  if (list.items.length === 0) {
    info('No alarms found')
    newline()
    return
  }

  const columns: TableColumn<AlarmResponse>[] = [
    { header: 'ID', key: 'id', width: 7 },
    { header: 'Severity', key: 'severity', color: (v) => severityColor(v) },
    { header: 'Status', key: 'status', color: (v) => statusColor(v) },
    { header: 'Type', key: 'alarm_type', color: (v) => colors.muted(v) },
    { header: 'Title', key: 'title', color: (v) => colors.bold(v) },
    { header: 'Fired', accessor: (a) => formatDate(a.fired_at), color: (v) => colors.muted(v) },
  ]
  printTable(list.items, columns, { style: 'minimal' })
  const pages = Math.max(1, Math.ceil(list.total / list.page_size))
  info(`Page ${list.page} of ${pages}`)
  newline()
}

async function summaryAction(options: SummaryOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = await resolveScope(options)

  const summary = await withSpinner('Fetching alarm summary...', async (): Promise<AlarmSummaryResponse> => {
    const { data, error } = scope.kind === 'system'
      ? await getSystemAlarmsSummary({ client })
      : await getProjectAlarmsSummary({ client, path: { project_id: scope.projectId } })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to get alarm summary')
    }
    return data
  })

  if (options.json) {
    json(summary)
    return
  }

  newline()
  header(`${icons.warning} Alarm summary for ${scopeLabel(scope)}`)
  keyValue('Active', summary.total_active)
  keyValue('Firing', summary.firing)
  keyValue('Acknowledged', summary.acknowledged)
  keyValue('Critical', summary.critical)
  keyValue('Warning', summary.warning)
  for (const [type, count] of Object.entries(summary.by_type).sort(([a], [b]) => a.localeCompare(b))) {
    keyValue(`  ${type}`, count)
  }
  newline()
}

async function sendBulk(scope: AlarmScope, body: BulkAlarmRequest): Promise<BulkAlarmResponse> {
  const { data, error } = scope.kind === 'system'
    ? await bulkUpdateSystemAlarms({ client, body })
    : await bulkUpdateProjectAlarms({ client, path: { project_id: scope.projectId }, body })
  if (error || !data) {
    throw new Error(getErrorMessage(error) ?? 'Bulk alarm update failed')
  }
  return data
}

async function bulkAction(
  action: BulkAlarmActionRequest,
  rawIds: string[],
  options: BulkOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()
  const body = buildBulkRequest(action, rawIds, options)
  const scope = await resolveScope(options)
  const verb = action === 'acknowledge' ? 'Acknowledg' : 'Resolv'

  const totals = { updated: 0, skipped: 0, remaining: 0, updated_ids: [] as number[] }
  await withSpinner(`${verb}ing alarms...`, async () => {
    // `--all` is capped server-side per request; keep going until nothing
    // matching is left, bounded so a still-firing source can't loop forever.
    for (let round = 0; round < MAX_BULK_ROUNDS; round++) {
      const result = await sendBulk(scope, body)
      totals.updated += result.updated
      totals.skipped += result.skipped
      totals.remaining = result.remaining
      totals.updated_ids.push(...result.updated_ids)
      if (result.remaining === 0 || result.updated === 0) break
    }
  })

  if (options.json) {
    json({ action, ...totals })
    return
  }

  newline()
  if (totals.updated === 0) {
    info(`No alarms needed ${action === 'acknowledge' ? 'acknowledging' : 'resolving'} in ${scopeLabel(scope)}`)
  } else {
    success(`${verb}ed ${totals.updated} alarm${totals.updated === 1 ? '' : 's'} in ${scopeLabel(scope)}`)
  }
  if (totals.skipped > 0) {
    info(`${totals.skipped} already ${action === 'acknowledge' ? 'acknowledged or resolved' : 'resolved'}`)
  }
  if (totals.remaining > 0) {
    warning(`${totals.remaining} matching alarm(s) remain — run the command again to continue`)
  }
  newline()
}
