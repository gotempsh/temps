import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listBackupSchedules,
  createBackupSchedule,
  deleteBackupSchedule,
  getBackupSchedule,
  listBackupsForSchedule,
  enableBackupSchedule,
  disableBackupSchedule,
  getBackup,
} from '../../api/sdk.gen.js'
import type { BackupScheduleResponse, BackupResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm, promptText, promptSelect } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, formatRelativeTime } from '../../ui/output.js'

export function registerBackupsCommands(program: Command): void {
  const backups = program
    .command('backups')
    .alias('backup')
    .description('Manage backup schedules and backups')

  // Schedule commands
  const schedules = backups
    .command('schedules')
    .alias('schedule')
    .description('Manage backup schedules')

  schedules
    .command('list')
    .alias('ls')
    .description('List backup schedules')
    .option('--json', 'Output in JSON format')
    .action(listSchedules)

  schedules
    .command('create')
    .description('Create a backup schedule')
    .action(createSchedule)

  schedules
    .command('show <schedule-id>')
    .description('Show backup schedule details')
    .option('--json', 'Output in JSON format')
    .action(showSchedule)

  schedules
    .command('enable <schedule-id>')
    .description('Enable a backup schedule')
    .action(enableSchedule)

  schedules
    .command('disable <schedule-id>')
    .description('Disable a backup schedule')
    .action(disableSchedule)

  schedules
    .command('delete <schedule-id>')
    .alias('rm')
    .description('Delete a backup schedule')
    .option('-f, --force', 'Skip confirmation')
    .action(deleteSchedule)

  // Backup commands
  backups
    .command('list <schedule-id>')
    .alias('ls')
    .description('List backups for a schedule')
    .option('--json', 'Output in JSON format')
    .action(listBackups)

  backups
    .command('show <backup-id>')
    .description('Show backup details')
    .option('--json', 'Output in JSON format')
    .action(showBackup)
}

async function listSchedules(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const schedules = await withSpinner('Fetching backup schedules...', async () => {
    const { data, error } = await listBackupSchedules({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(schedules)
    return
  }

  newline()
  header(`${icons.package} Backup Schedules (${schedules.length})`)

  if (schedules.length === 0) {
    info('No backup schedules configured')
    info('Run: temps backups schedules create')
    newline()
    return
  }

  const columns: TableColumn<BackupScheduleResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'backup_type' },
    { header: 'Schedule', key: 'schedule_expression' },
    { header: 'Retention', accessor: (s) => `${s.retention_period} days` },
    { header: 'Status', accessor: (s) => s.enabled ? 'enabled' : 'disabled', color: (v) => statusBadge(v) },
  ]

  printTable(schedules, columns, { style: 'minimal' })
  newline()
}

async function createSchedule(): Promise<void> {
  await requireAuth()
  await setupClient()

  const name = await promptText({
    message: 'Schedule name',
    required: true,
  })

  const backupType = await promptSelect({
    message: 'Backup type',
    choices: [
      { name: 'Full', value: 'full' },
      { name: 'Incremental', value: 'incremental' },
    ],
  })

  const scheduleExpression = await promptText({
    message: 'Schedule expression (cron format, e.g., 0 2 * * * for daily at 2 AM)',
    default: '0 2 * * *',
    required: true,
  })

  const retentionDays = await promptText({
    message: 'Retention period (days)',
    default: '30',
  })

  const description = await promptText({
    message: 'Description (optional)',
    default: '',
  })

  const s3SourceId = await promptText({
    message: 'S3 Source ID',
    required: true,
  })

  const schedule = await withSpinner('Creating backup schedule...', async () => {
    const { data, error } = await createBackupSchedule({
      client,
      body: {
        name,
        backup_type: backupType,
        schedule_expression: scheduleExpression,
        retention_period: parseInt(retentionDays, 10),
        description: description || null,
        s3_source_id: parseInt(s3SourceId, 10),
        enabled: true,
        tags: [],
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Backup schedule #${schedule.id} created`)
  info(`Enable/disable with: temps backups schedules enable/disable ${schedule.id}`)
}

async function showSchedule(scheduleId: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(scheduleId, 10)
  if (isNaN(id)) {
    warning('Invalid schedule ID')
    return
  }

  const schedule = await withSpinner('Fetching schedule...', async () => {
    const { data, error } = await getBackupSchedule({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Schedule ${scheduleId} not found`)
    }
    return data
  })

  if (options.json) {
    json(schedule)
    return
  }

  newline()
  header(`${icons.package} Backup Schedule #${schedule.id}`)
  console.log(`  ${colors.muted('Name:')} ${schedule.name}`)
  console.log(`  ${colors.muted('Type:')} ${schedule.backup_type}`)
  console.log(`  ${colors.muted('Schedule:')} ${schedule.schedule_expression}`)
  console.log(`  ${colors.muted('Retention:')} ${schedule.retention_period} days`)
  console.log(`  ${colors.muted('Status:')} ${statusBadge(schedule.enabled ? 'enabled' : 'disabled')}`)
  if (schedule.description) {
    console.log(`  ${colors.muted('Description:')} ${schedule.description}`)
  }
  console.log(`  ${colors.muted('S3 Source ID:')} ${schedule.s3_source_id}`)
  if (schedule.last_run) {
    console.log(`  ${colors.muted('Last Run:')} ${formatRelativeTime(new Date(schedule.last_run * 1000).toISOString())}`)
  }
  if (schedule.next_run) {
    console.log(`  ${colors.muted('Next Run:')} ${formatRelativeTime(new Date(schedule.next_run * 1000).toISOString())}`)
  }
  newline()
}

async function enableSchedule(scheduleId: string): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(scheduleId, 10)
  if (isNaN(id)) {
    warning('Invalid schedule ID')
    return
  }

  await withSpinner('Enabling schedule...', async () => {
    const { error } = await enableBackupSchedule({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Schedule #${scheduleId} enabled`)
}

async function disableSchedule(scheduleId: string): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(scheduleId, 10)
  if (isNaN(id)) {
    warning('Invalid schedule ID')
    return
  }

  await withSpinner('Disabling schedule...', async () => {
    const { error } = await disableBackupSchedule({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Schedule #${scheduleId} disabled`)
}

async function deleteSchedule(scheduleId: string, options: { force?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(scheduleId, 10)
  if (isNaN(id)) {
    warning('Invalid schedule ID')
    return
  }

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Delete backup schedule #${scheduleId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting schedule...', async () => {
    const { error } = await deleteBackupSchedule({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Schedule #${scheduleId} deleted`)
}

async function listBackups(scheduleId: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(scheduleId, 10)
  if (isNaN(id)) {
    warning('Invalid schedule ID')
    return
  }

  const backups = await withSpinner('Fetching backups...', async () => {
    const { data, error } = await listBackupsForSchedule({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(backups)
    return
  }

  newline()
  header(`${icons.package} Backups for Schedule #${scheduleId} (${backups.length})`)

  if (backups.length === 0) {
    info('No backups found for this schedule')
    newline()
    return
  }

  const columns: TableColumn<BackupResponse>[] = [
    { header: 'ID', key: 'backup_id', width: 12 },
    { header: 'Type', key: 'backup_type' },
    { header: 'State', key: 'state', color: (v) => statusBadge(v) },
    { header: 'Size', accessor: (b) => formatBytes(b.size_bytes) },
    { header: 'Started', accessor: (b) => formatRelativeTime(new Date(b.started_at * 1000).toISOString()), color: (v) => colors.muted(v) },
  ]

  printTable(backups, columns, { style: 'minimal' })
  newline()
}

async function showBackup(backupId: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const backup = await withSpinner('Fetching backup...', async () => {
    const { data, error } = await getBackup({
      client,
      path: { id: backupId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Backup ${backupId} not found`)
    }
    return data
  })

  if (options.json) {
    json(backup)
    return
  }

  newline()
  header(`${icons.package} Backup ${backup.backup_id}`)
  console.log(`  ${colors.muted('Name:')} ${backup.name}`)
  console.log(`  ${colors.muted('Type:')} ${backup.backup_type}`)
  console.log(`  ${colors.muted('State:')} ${statusBadge(backup.state)}`)
  console.log(`  ${colors.muted('Compression:')} ${backup.compression_type}`)
  console.log(`  ${colors.muted('Size:')} ${formatBytes(backup.size_bytes)}`)
  if (backup.checksum) {
    console.log(`  ${colors.muted('Checksum:')} ${backup.checksum}`)
  }
  console.log(`  ${colors.muted('Started:')} ${formatRelativeTime(new Date(backup.started_at * 1000).toISOString())}`)
  if (backup.completed_at) {
    console.log(`  ${colors.muted('Completed:')} ${formatRelativeTime(new Date(backup.completed_at * 1000).toISOString())}`)
  }
  if (backup.schedule_id) {
    console.log(`  ${colors.muted('Schedule ID:')} ${backup.schedule_id}`)
  }
  console.log(`  ${colors.muted('S3 Location:')} ${backup.s3_location}`)
  if (backup.error_message) {
    console.log(`  ${colors.muted('Error:')} ${colors.error(backup.error_message)}`)
  }
  newline()
}

function formatBytes(bytes?: number | null): string {
  if (bytes === undefined || bytes === null) return '-'
  if (bytes === 0) return '0 B'

  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  const size = bytes / Math.pow(1024, i)

  return `${size.toFixed(1)} ${units[i]}`
}
