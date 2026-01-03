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

interface CreateScheduleOptions {
  name?: string
  type?: string
  schedule?: string
  retention?: string
  description?: string
  s3SourceId?: string
  yes?: boolean
}

interface ShowScheduleOptions {
  id: string
  json?: boolean
}

interface EnableDisableOptions {
  id: string
}

interface DeleteScheduleOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface ListBackupsOptions {
  scheduleId: string
  json?: boolean
}

interface ShowBackupOptions {
  id: string
  json?: boolean
}

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
    .option('-n, --name <name>', 'Schedule name')
    .option('-t, --type <type>', 'Backup type (full, incremental)')
    .option('-s, --schedule <cron>', 'Schedule expression (cron format)')
    .option('-r, --retention <days>', 'Retention period in days')
    .option('-d, --description <desc>', 'Description')
    .option('--s3-source-id <id>', 'S3 Source ID')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createSchedule)

  schedules
    .command('show')
    .description('Show backup schedule details')
    .requiredOption('--id <id>', 'Schedule ID')
    .option('--json', 'Output in JSON format')
    .action(showSchedule)

  schedules
    .command('enable')
    .description('Enable a backup schedule')
    .requiredOption('--id <id>', 'Schedule ID')
    .action(enableSchedule)

  schedules
    .command('disable')
    .description('Disable a backup schedule')
    .requiredOption('--id <id>', 'Schedule ID')
    .action(disableSchedule)

  schedules
    .command('delete')
    .alias('rm')
    .description('Delete a backup schedule')
    .requiredOption('--id <id>', 'Schedule ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(deleteSchedule)

  // Backup commands
  backups
    .command('list')
    .alias('ls')
    .description('List backups for a schedule')
    .requiredOption('--schedule-id <id>', 'Schedule ID')
    .option('--json', 'Output in JSON format')
    .action(listBackups)

  backups
    .command('show')
    .description('Show backup details')
    .requiredOption('--id <id>', 'Backup ID')
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
    info('Run: temps backups schedules create --name daily-backup --type full --schedule "0 2 * * *" --retention 30 --s3-source-id 1 -y')
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

async function createSchedule(options: CreateScheduleOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let name: string
  let backupType: string
  let scheduleExpression: string
  let retentionDays: number
  let description: string | null = null
  let s3SourceId: number

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.name && options.type && options.schedule && options.retention && options.s3SourceId

  if (isAutomation) {
    name = options.name!
    backupType = options.type!
    scheduleExpression = options.schedule!
    retentionDays = parseInt(options.retention!, 10)
    description = options.description || null
    s3SourceId = parseInt(options.s3SourceId!, 10)

    if (backupType !== 'full' && backupType !== 'incremental') {
      warning(`Invalid backup type: ${backupType}. Supported: full, incremental`)
      return
    }

    if (isNaN(retentionDays) || retentionDays <= 0) {
      warning('Invalid retention period')
      return
    }

    if (isNaN(s3SourceId)) {
      warning('Invalid S3 Source ID')
      return
    }
  } else {
    // Interactive mode
    name = options.name || await promptText({
      message: 'Schedule name',
      required: true,
    })

    backupType = options.type || await promptSelect({
      message: 'Backup type',
      choices: [
        { name: 'Full', value: 'full' },
        { name: 'Incremental', value: 'incremental' },
      ],
    })

    scheduleExpression = options.schedule || await promptText({
      message: 'Schedule expression (cron format, e.g., 0 2 * * * for daily at 2 AM)',
      default: '0 2 * * *',
      required: true,
    })

    const retentionInput = options.retention || await promptText({
      message: 'Retention period (days)',
      default: '30',
    })
    retentionDays = parseInt(retentionInput, 10)

    description = options.description || await promptText({
      message: 'Description (optional)',
      default: '',
    }) || null

    const s3Input = options.s3SourceId || await promptText({
      message: 'S3 Source ID',
      required: true,
    })
    s3SourceId = parseInt(s3Input, 10)
  }

  const schedule = await withSpinner('Creating backup schedule...', async () => {
    const { data, error } = await createBackupSchedule({
      client,
      body: {
        name,
        backup_type: backupType,
        schedule_expression: scheduleExpression,
        retention_period: retentionDays,
        description,
        s3_source_id: s3SourceId,
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
  info(`Enable/disable with: temps backups schedules enable/disable --id ${schedule.id}`)
}

async function showSchedule(options: ShowScheduleOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
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
      throw new Error(getErrorMessage(error) ?? `Schedule ${options.id} not found`)
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

async function enableSchedule(options: EnableDisableOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
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

  success(`Schedule #${options.id} enabled`)
}

async function disableSchedule(options: EnableDisableOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
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

  success(`Schedule #${options.id} disabled`)
}

async function deleteSchedule(options: DeleteScheduleOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid schedule ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete backup schedule #${options.id}?`,
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

  success(`Schedule #${options.id} deleted`)
}

async function listBackups(options: ListBackupsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.scheduleId, 10)
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
  header(`${icons.package} Backups for Schedule #${options.scheduleId} (${backups.length})`)

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

async function showBackup(options: ShowBackupOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const backup = await withSpinner('Fetching backup...', async () => {
    const { data, error } = await getBackup({
      client,
      path: { id: options.id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Backup ${options.id} not found`)
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
