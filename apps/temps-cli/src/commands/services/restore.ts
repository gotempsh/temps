import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import {
  getRestoreCapabilities,
  listRestoreRunsForService,
  startRestore,
  getRestoreRun,
  listSourceBackups,
  getService,
} from '../../api/sdk.gen.js'
import type {
  RestoreCapabilitiesResponse,
  RestoreRunView,
  SourceBackupEntry,
  RestoreRequestMode,
} from '../../api/types.gen.js'
import {
  startSpinner,
  succeedSpinner,
  failSpinner,
  updateSpinner,
  withSpinner,
} from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json as jsonOut,
  colors,
  success,
  info,
  warning,
  error as errorOutput,
  keyValue,
  formatRelativeTime,
} from '../../ui/output.js'

// ---- Action option types -------------------------------------------------

interface ShowCapsOptions {
  id: string
  json?: boolean
}

interface ListBackupsOptions {
  s3SourceId: string
  json?: boolean
}

interface RestoreOptions {
  id: string // source service id
  backupId?: string
  newService?: string // if set, clone-to-new-service mode
  pitr?: string // if set, PITR mode; value is ISO 8601 timestamp
  // --new-service may be combined with --pitr to route PITR into a new svc.
  yes?: boolean
  noWait?: boolean
  json?: boolean
}

interface RestoreRunsOptions {
  id: string
  json?: boolean
}

interface RestoreRunShowOptions {
  id: string
  json?: boolean
}

// ---- Actions -------------------------------------------------------------

async function showCapsAction(options: ShowCapsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const serviceId = parseInt(options.id, 10)
  if (!Number.isFinite(serviceId)) {
    errorOutput(`Invalid service id: ${options.id}`)
    process.exit(1)
  }

  const caps = await withSpinner('Fetching restore capabilities...', async () => {
    const { data, error } = await getRestoreCapabilities({ path: { id: serviceId } })
    if (error) throw new Error(getErrorMessage(error))
    return data as RestoreCapabilitiesResponse
  })

  if (options.json) {
    jsonOut(caps)
    return
  }

  newline()
  header(`${icons.info} Restore capabilities`)
  keyValue('In-place', caps.restore_in_place ? colors.success('yes') : colors.muted('no'))
  keyValue(
    'Clone to new service',
    caps.restore_to_new_service ? colors.success('yes') : colors.muted('no'),
  )
  keyValue('Point-in-time recovery', caps.pitr ? colors.success('yes') : colors.muted('no'))
  if (caps.earliest_pitr_time) {
    keyValue('PITR earliest', String(caps.earliest_pitr_time))
  }
  if (caps.latest_pitr_time) {
    keyValue('PITR latest', String(caps.latest_pitr_time))
  }
  keyValue('Suggested new-service name', caps.suggested_new_service_name)
  newline()
}

async function listBackupsAction(options: ListBackupsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const s3SourceId = parseInt(options.s3SourceId, 10)
  if (!Number.isFinite(s3SourceId)) {
    errorOutput(`Invalid S3 source id: ${options.s3SourceId}`)
    process.exit(1)
  }

  const index = await withSpinner('Fetching backups from S3 source...', async () => {
    const { data, error } = await listSourceBackups({ path: { id: s3SourceId } })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const entries: SourceBackupEntry[] = (index as { backups: SourceBackupEntry[] }).backups ?? []

  if (options.json) {
    jsonOut(index)
    return
  }

  if (entries.length === 0) {
    info('No backups found on this S3 source.')
    return
  }

  const columns: TableColumn<SourceBackupEntry>[] = [
    { header: 'ID', accessor: (r) => String(r.id ?? '') },
    { header: 'UUID', accessor: (r) => String(r.backup_id ?? '') },
    { header: 'Name', accessor: (r) => r.name ?? '' },
    { header: 'Type', accessor: (r) => r.backup_type ?? '' },
    {
      header: 'Size',
      accessor: (r) =>
        typeof r.size_bytes === 'number' && r.size_bytes != null
          ? formatBytes(r.size_bytes)
          : '—',
      color: (value, r) => typeof r.size_bytes === 'number' && r.size_bytes != null ? value : colors.muted(value),
    },
    {
      header: 'Location',
      accessor: (r) =>
        (r.location ?? '').startsWith('s3://')
          ? 'WAL-G'
          : 'legacy',
      color: (value, r) => (r.location ?? '').startsWith('s3://') ? colors.success(value) : colors.muted(value),
    },
    {
      header: 'Created',
      accessor: (r) => (r.created_at ? formatRelativeTime(String(r.created_at)) : ''),
    },
  ]
  newline()
  header(`${icons.info} Backups on S3 source ${s3SourceId}`)
  printTable(entries, columns)
  newline()
}

async function restoreAction(options: RestoreOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const serviceId = parseInt(options.id, 10)
  if (!Number.isFinite(serviceId)) {
    errorOutput(`Invalid service id: ${options.id}`)
    process.exit(1)
  }

  // Step 1: fetch capabilities + service name (for the auto-suggestion).
  const { capabilities, suggestedName, serviceName, serviceType } = await withSpinner(
    'Fetching service metadata...',
    async () => {
      const [capsResp, svcResp] = await Promise.all([
        getRestoreCapabilities({ path: { id: serviceId } }),
        getService({ path: { id: serviceId } }),
      ])
      if (capsResp.error) throw new Error(getErrorMessage(capsResp.error))
      if (svcResp.error) throw new Error(getErrorMessage(svcResp.error))
      const caps = capsResp.data as RestoreCapabilitiesResponse
      const svc = svcResp.data as { name?: string; service_type?: string }
      return {
        capabilities: caps,
        suggestedName: caps.suggested_new_service_name,
        serviceName: svc.name ?? `service ${serviceId}`,
        serviceType: svc.service_type ?? '',
      }
    },
  )

  // Step 2: resolve the backup to restore from.
  const backupId = options.backupId ? parseInt(options.backupId, 10) : await askForBackupId()
  if (!Number.isFinite(backupId)) {
    errorOutput('Backup id is required.')
    process.exit(1)
  }

  // Step 3: determine mode and gather extra parameters.
  let mode: RestoreRequestMode
  let targetName: string | undefined

  const wantsPitr = options.pitr != null
  const wantsNew = options.newService != null

  if (wantsPitr) {
    if (!capabilities.pitr) {
      errorOutput(
        `Service '${serviceName}' (${serviceType}) does not support point-in-time recovery.`,
      )
      process.exit(1)
    }
    const targetTime = new Date(options.pitr!)
    if (Number.isNaN(targetTime.getTime())) {
      errorOutput(`Invalid --pitr timestamp: '${options.pitr}'. Expected ISO 8601.`)
      process.exit(1)
    }
    const toNew = wantsNew
    if (toNew) {
      targetName = await resolveNewServiceName(options.newService!, suggestedName)
    }
    mode = {
      mode: 'pitr',
      to_new_service: toNew,
      new_service_name: toNew ? targetName : undefined,
      target: { kind: 'time', time: targetTime.toISOString() },
    }
  } else if (wantsNew) {
    if (!capabilities.restore_to_new_service) {
      errorOutput(
        `Service '${serviceName}' (${serviceType}) does not support restoring to a new service.`,
      )
      process.exit(1)
    }
    targetName = await resolveNewServiceName(options.newService!, suggestedName)
    mode = { mode: 'new_service', name: targetName, parameter_overrides: {} }
  } else {
    if (!capabilities.restore_in_place) {
      errorOutput(
        `Service '${serviceName}' (${serviceType}) does not support in-place restore.`,
      )
      process.exit(1)
    }
    mode = { mode: 'in_place' }
  }

  // Step 4: confirmation.
  if (!options.yes) {
    const modeLabel = describeRestoreMode(mode, serviceName, targetName)
    newline()
    header(`${icons.arrow} ${modeLabel}`)
    keyValue('Source service', `${serviceName} (id ${serviceId}, ${serviceType})`)
    keyValue('Backup ID', String(backupId))
    if (mode.mode === 'pitr') {
      keyValue('Target time', mode.target.kind === 'time' ? mode.target.time : '')
    }
    newline()
    const go = await promptConfirm({
      message: describeRestoreConfirmPrompt(mode, serviceName),
      default: false,
    })
    if (!go) {
      warning('Aborted.')
      return
    }
  }

  // Step 5: kick off the restore.
  const run = await withSpinner('Starting restore...', async () => {
    const { data, error } = await startRestore({
      path: { id: serviceId },
      body: {
        backup_id: backupId,
        ...mode,
      } as unknown as RestoreRequestMode & { backup_id: number },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as RestoreRunView
  })

  if (options.json) {
    jsonOut(run)
    return
  }

  success(`Restore run ${run.id} started (status=${run.status}, phase=${run.phase})`)

  // Step 6: poll until terminal (unless --no-wait).
  if (options.noWait) {
    info(`Run in background. Poll with: bunx @temps-sdk/cli services restore-run ${run.id}`)
    return
  }

  const finalRun = await pollRestoreRun(run.id)

  if (finalRun.status === 'completed') {
    success(`Restore completed (run ${finalRun.id}).`)
    if (finalRun.target_service_id != null) {
      info(`New service id: ${finalRun.target_service_id}`)
    }
  } else {
    errorOutput(`Restore failed (run ${finalRun.id}): ${finalRun.error_message ?? 'unknown error'}`)
    process.exitCode = 1
  }
}

async function listRunsAction(options: RestoreRunsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const serviceId = parseInt(options.id, 10)
  if (!Number.isFinite(serviceId)) {
    errorOutput(`Invalid service id: ${options.id}`)
    process.exit(1)
  }

  const runs = await withSpinner('Fetching restore runs...', async () => {
    const { data, error } = await listRestoreRunsForService({ path: { id: serviceId } })
    if (error) throw new Error(getErrorMessage(error))
    return (data ?? []) as RestoreRunView[]
  })

  if (options.json) {
    jsonOut(runs)
    return
  }

  if (runs.length === 0) {
    info('No restore runs for this service yet.')
    return
  }

  const columns: TableColumn<RestoreRunView>[] = [
    { header: 'ID', accessor: (r) => String(r.id) },
    { header: 'Mode', accessor: (r) => r.mode },
    { header: 'Phase', accessor: (r) => r.phase },
    {
      header: 'Status',
      accessor: (r) => r.status,
      color: (_value, r) => statusBadge(
        r.status === 'completed' ? 'active' : r.status === 'failed' ? 'inactive' : 'pending',
      ),
    },
    {
      header: 'Target',
      accessor: (r) =>
        r.target_service_id != null
          ? `#${r.target_service_id} (${r.target_service_name ?? ''})`
          : '—',
      color: (value, r) => r.target_service_id != null ? value : colors.muted(value),
    },
    {
      header: 'Started',
      accessor: (r) => (r.started_at ? formatRelativeTime(r.started_at) : ''),
    },
  ]
  newline()
  header(`${icons.info} Restore runs for service ${serviceId}`)
  printTable(runs, columns)
  newline()
}

async function showRunAction(options: RestoreRunShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const runId = parseInt(options.id, 10)
  if (!Number.isFinite(runId)) {
    errorOutput(`Invalid run id: ${options.id}`)
    process.exit(1)
  }

  const run = await withSpinner('Fetching restore run...', async () => {
    const { data, error } = await getRestoreRun({ path: { id: runId } })
    if (error) throw new Error(getErrorMessage(error))
    return data as RestoreRunView
  })

  if (options.json) {
    jsonOut(run)
    return
  }

  newline()
  header(`${icons.info} Restore run ${run.id}`)
  keyValue('Mode', run.mode)
  keyValue('Status', run.status)
  keyValue('Phase', run.phase)
  keyValue('Source backup ID', String(run.source_backup_id))
  keyValue('Source service ID', String(run.source_service_id))
  if (run.target_service_id != null) {
    keyValue(
      'Target service',
      `#${run.target_service_id} (${run.target_service_name ?? ''})`,
    )
  }
  if (run.started_at) keyValue('Started', run.started_at)
  if (run.finished_at) keyValue('Finished', run.finished_at)
  if (run.error_message) keyValue('Error', colors.error(run.error_message))
  newline()
}

// ---- Helpers -------------------------------------------------------------

/**
 * Describe the confirmation header shown before a restore runs. Getting the
 * DESTRUCTIVE/OVERWRITE wording right matters: it's the only warning a human
 * operator sees before data on the source service is replaced in place.
 */
export function describeRestoreMode(
  mode: RestoreRequestMode,
  serviceName: string,
  targetName?: string,
): string {
  return mode.mode === 'in_place'
    ? `Restore in place (DESTRUCTIVE) onto '${serviceName}'`
    : mode.mode === 'new_service'
      ? `Clone into new service '${targetName}'`
      : mode.to_new_service
        ? `Point-in-time recovery → new service '${targetName}'`
        : `Point-in-time recovery (DESTRUCTIVE) onto '${serviceName}'`
}

/** Only in-place restores (or PITR that stays in place) overwrite the source
 *  service, so only those get the OVERWRITE warning in the confirm prompt. */
export function describeRestoreConfirmPrompt(mode: RestoreRequestMode, serviceName: string): string {
  return mode.mode === 'in_place' || (mode.mode === 'pitr' && !mode.to_new_service)
    ? `This will OVERWRITE data on '${serviceName}'. Continue?`
    : `Proceed with restore?`
}

async function askForBackupId(): Promise<number> {
  const value = await promptText({
    message: 'Backup ID to restore from:',
    required: true,
    validate: (v) => (/^\d+$/.test(v.trim()) ? true : 'must be a positive integer'),
  })
  return parseInt(value, 10)
}

async function resolveNewServiceName(
  raw: string,
  suggested: string,
): Promise<string> {
  // Treat empty/"-"/"auto" as "use the suggested name without prompting"
  const trimmed = raw.trim()
  if (trimmed && trimmed !== '-' && trimmed.toLowerCase() !== 'auto') {
    return trimmed
  }
  return await promptText({
    message: 'New service name:',
    default: suggested,
    required: true,
    validate: (v) => (v.trim().length > 0 ? true : 'cannot be empty'),
  })
}

async function pollRestoreRun(runId: number): Promise<RestoreRunView> {
  startSpinner('Waiting for restore to finish...')
  const maxAttempts = 600 // 20 minutes at 2s cadence
  let lastPhase = ''
  for (let i = 0; i < maxAttempts; i++) {
    try {
      const { data, error } = await getRestoreRun({ path: { id: runId } })
      if (error) {
        // Surface the error but keep polling a few more cycles in case of
        // transient auth blip — stop on persistent failure.
        if (i > 5) {
          failSpinner(`Failed to fetch run status: ${getErrorMessage(error)}`)
          throw new Error(getErrorMessage(error))
        }
      } else if (data) {
        const run = data as RestoreRunView
        if (run.phase !== lastPhase) {
          lastPhase = run.phase
          updateSpinner(`Phase: ${run.phase}`)
        }
        if (run.status === 'completed' || run.status === 'failed') {
          if (run.status === 'completed') succeedSpinner(`Phase: ${run.phase}`)
          else failSpinner(`Phase: ${run.phase}`)
          return run
        }
      }
    } catch (e) {
      failSpinner(`Restore poll error: ${e instanceof Error ? e.message : String(e)}`)
      throw e
    }
    await new Promise((r) => setTimeout(r, 2000))
  }
  failSpinner('Timed out waiting for restore to finish.')
  throw new Error('Restore poll timeout')
}

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

// ---- Registration --------------------------------------------------------

export function registerRestoreCommands(services: Command): void {
  services
    .command('restore-capabilities')
    .description('Show what restore modes a service supports (in-place / new service / PITR)')
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(showCapsAction)

  services
    .command('list-backups')
    .description('List backups stored on an S3 source')
    .requiredOption('--s3-source-id <id>', 'S3 source ID')
    .option('--json', 'Output in JSON format')
    .action(listBackupsAction)

  services
    .command('restore')
    .description('Restore a service from a backup (in-place, new service, or PITR)')
    .requiredOption('--id <id>', 'Source service ID (the service the backup came from)')
    .requiredOption('--backup-id <id>', 'Backup ID to restore from (see `list-backups`)')
    .option(
      '--new-service [name]',
      'Clone into a new service. Omit the value or pass "auto" to accept the auto-suggested name.',
    )
    .option(
      '--pitr <iso>',
      'Point-in-time recovery target, ISO 8601 timestamp (requires WAL-G backup). Combine with --new-service to route PITR into a new service.',
    )
    .option('-y, --yes', 'Skip confirmation')
    .option('--no-wait', 'Return immediately without polling run status')
    .option('--json', 'Output in JSON format')
    .action(restoreAction)

  services
    .command('restore-runs')
    .description('List recent restore runs for a service')
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(listRunsAction)

  services
    .command('restore-run')
    .description('Show a single restore run')
    .requiredOption('--id <id>', 'Restore run ID')
    .option('--json', 'Output in JSON format')
    .action(showRunAction)
}
