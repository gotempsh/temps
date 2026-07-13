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
  listS3Sources,
  createS3Source,
  getS3Source,
  updateS3Source,
  deleteS3Source,
  listSourceBackups,
  runBackupForSource,
  runExternalServiceBackup,
} from '../../api/sdk.gen.js'
import type { BackupScheduleResponse, BackupResponse, S3SourceResponse, SourceBackupEntry } from '../../api/types.gen.js'
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

interface DeleteBackupOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface RetentionCleanupReport {
  dry_run: boolean
  schedule_id: number | null
  expired: number
  deleted: number
  failed: number
  failures: Array<{ backup_id: string; reason: string }>
  candidate_backup_ids: string[]
  candidate_backup_ids_truncated: boolean
}

interface CleanupBackupsOptions {
  dryRun?: boolean
  scheduleId?: string
  yes?: boolean
  json?: boolean
}

interface CreateSourceOptions {
  name?: string
  bucket?: string
  region?: string
  endpoint?: string
  accessKey?: string
  secretKey?: string
  prefix?: string
  yes?: boolean
}

interface ShowSourceOptions {
  id: string
  json?: boolean
}

interface UpdateSourceOptions {
  id: string
  name?: string
  bucket?: string
  region?: string
  endpoint?: string
  accessKey?: string
  secretKey?: string
  prefix?: string
}

interface DeleteSourceOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface SourceBackupsOptions {
  id: string
  json?: boolean
}

interface RunSourceBackupOptions {
  id: string
}

interface RunServiceBackupOptions {
  id: string
  s3SourceId: string
  type?: string
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

  // S3 Source commands
  const sources = backups
    .command('sources')
    .alias('source')
    .description('Manage S3 backup sources')

  sources
    .command('list')
    .alias('ls')
    .description('List S3 sources')
    .option('--json', 'Output in JSON format')
    .action(listSources)

  sources
    .command('create')
    .description('Create an S3 source')
    .option('-n, --name <name>', 'Source name')
    .option('--bucket <bucket>', 'S3 bucket name')
    .option('--region <region>', 'S3 region')
    .option('--endpoint <endpoint>', 'S3 endpoint (for S3-compatible services)')
    .option('--access-key <key>', 'Access key ID')
    .option('--secret-key <key>', 'Secret access key')
    .option('--prefix <prefix>', 'Bucket path/prefix')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createSource)

  sources
    .command('show')
    .description('Show S3 source details')
    .requiredOption('--id <id>', 'S3 source ID')
    .option('--json', 'Output in JSON format')
    .action(showSource)

  sources
    .command('update')
    .description('Update an S3 source')
    .requiredOption('--id <id>', 'S3 source ID')
    .option('-n, --name <name>', 'New source name')
    .option('--bucket <bucket>', 'New S3 bucket name')
    .option('--region <region>', 'New S3 region')
    .option('--endpoint <endpoint>', 'New S3 endpoint')
    .option('--access-key <key>', 'New access key ID')
    .option('--secret-key <key>', 'New secret access key')
    .option('--prefix <prefix>', 'New bucket path/prefix')
    .action(updateSource)

  sources
    .command('remove')
    .alias('rm')
    .description('Delete an S3 source')
    .requiredOption('--id <id>', 'S3 source ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(deleteSource)

  sources
    .command('backups')
    .description('List backups for an S3 source')
    .requiredOption('--id <id>', 'S3 source ID')
    .option('--json', 'Output in JSON format')
    .action(listSourceBackupsFn)

  sources
    .command('run')
    .description('Trigger a backup for an S3 source')
    .requiredOption('--id <id>', 'S3 source ID')
    .action(runSourceBackup)

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

  backups
    .command('delete')
    .alias('rm')
    .description('Permanently delete one terminal backup')
    .requiredOption('--id <id>', 'Backup UUID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(deleteBackup)

  backups
    .command('cleanup')
    .description('Delete backups expired by their schedule retention policy')
    .option('--dry-run', 'Preview expired backups without deleting them')
    .option('--schedule-id <id>', 'Limit cleanup to one schedule')
    .option('-y, --yes', 'Skip confirmation prompt')
    .option('--json', 'Output the cleanup report as JSON')
    .action(cleanupBackups)

  // Run backup for external service
  backups
    .command('run-service')
    .description('Run a backup for an external service')
    .requiredOption('--id <id>', 'External service ID')
    .requiredOption('--s3-source-id <id>', 'S3 source ID to store the backup')
    .option('-t, --type <type>', 'Backup type (e.g., full, incremental)')
    .action(runServiceBackup)
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

// S3 Source commands

async function listSources(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const sources = await withSpinner('Fetching S3 sources...', async () => {
    const { data, error } = await listS3Sources({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(sources)
    return
  }

  newline()
  header(`${icons.package} S3 Sources (${sources.length})`)

  if (sources.length === 0) {
    info('No S3 sources configured')
    info('Run: temps backups sources create --name my-s3 --bucket my-bucket --region us-east-1 --access-key AKIA... --secret-key ... -y')
    newline()
    return
  }

  const columns: TableColumn<S3SourceResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Bucket', key: 'bucket_name' },
    { header: 'Region', key: 'region' },
    { header: 'Path', key: 'bucket_path' },
    { header: 'Endpoint', accessor: (s) => s.endpoint ?? '-', color: (v) => colors.muted(v) },
  ]

  printTable(sources, columns, { style: 'minimal' })
  newline()
}

async function createSource(options: CreateSourceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let name: string
  let bucketName: string
  let region: string
  let endpoint: string | null = null
  let accessKeyId: string
  let secretKey: string
  let bucketPath: string

  const isAutomation = options.yes && options.name && options.bucket && options.region && options.accessKey && options.secretKey

  if (isAutomation) {
    name = options.name!
    bucketName = options.bucket!
    region = options.region!
    endpoint = options.endpoint || null
    accessKeyId = options.accessKey!
    secretKey = options.secretKey!
    bucketPath = options.prefix || ''
  } else {
    name = options.name || await promptText({
      message: 'Source name',
      required: true,
    })

    bucketName = options.bucket || await promptText({
      message: 'S3 bucket name',
      required: true,
    })

    region = options.region || await promptText({
      message: 'S3 region',
      default: 'us-east-1',
      required: true,
    })

    endpoint = options.endpoint || await promptText({
      message: 'S3 endpoint (optional, for S3-compatible services)',
      default: '',
    }) || null

    accessKeyId = options.accessKey || await promptText({
      message: 'Access key ID',
      required: true,
    })

    secretKey = options.secretKey || await promptText({
      message: 'Secret access key',
      required: true,
    })

    bucketPath = options.prefix || await promptText({
      message: 'Bucket path/prefix (optional)',
      default: '',
    })
  }

  const source = await withSpinner('Creating S3 source...', async () => {
    const { data, error } = await createS3Source({
      client,
      body: {
        name,
        bucket_name: bucketName,
        region,
        endpoint,
        access_key_id: accessKeyId,
        secret_key: secretKey,
        bucket_path: bucketPath,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`S3 source #${source.id} created`)
  info(`Use with schedules: temps backups schedules create --s3-source-id ${source.id} ...`)
}

async function showSource(options: ShowSourceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid S3 source ID')
    return
  }

  const source = await withSpinner('Fetching S3 source...', async () => {
    const { data, error } = await getS3Source({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `S3 source ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(source)
    return
  }

  newline()
  header(`${icons.package} S3 Source #${source.id}`)
  console.log(`  ${colors.muted('Name:')} ${source.name}`)
  console.log(`  ${colors.muted('Bucket:')} ${source.bucket_name}`)
  console.log(`  ${colors.muted('Region:')} ${source.region}`)
  console.log(`  ${colors.muted('Path:')} ${source.bucket_path || '-'}`)
  if (source.endpoint) {
    console.log(`  ${colors.muted('Endpoint:')} ${source.endpoint}`)
  }
  console.log(`  ${colors.muted('Access Key:')} ${maskSecret(source.access_key_id)}`)
  if (source.force_path_style !== null && source.force_path_style !== undefined) {
    console.log(`  ${colors.muted('Force Path Style:')} ${source.force_path_style}`)
  }
  console.log(`  ${colors.muted('Created:')} ${formatRelativeTime(new Date(source.created_at * 1000).toISOString())}`)
  console.log(`  ${colors.muted('Updated:')} ${formatRelativeTime(new Date(source.updated_at * 1000).toISOString())}`)
  newline()
}

async function updateSource(options: UpdateSourceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid S3 source ID')
    return
  }

  const hasChanges = options.name || options.bucket || options.region || options.endpoint || options.accessKey || options.secretKey || options.prefix
  if (!hasChanges) {
    warning('No update options provided. Use --name, --bucket, --region, --endpoint, --access-key, --secret-key, or --prefix')
    return
  }

  const source = await withSpinner('Updating S3 source...', async () => {
    const { data, error } = await updateS3Source({
      client,
      path: { id },
      body: {
        name: options.name || null,
        bucket_name: options.bucket || null,
        region: options.region || null,
        endpoint: options.endpoint || null,
        access_key_id: options.accessKey || null,
        secret_key: options.secretKey || null,
        bucket_path: options.prefix || null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`S3 source #${source.id} updated`)
}

async function deleteSource(options: DeleteSourceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid S3 source ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete S3 source #${options.id}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting S3 source...', async () => {
    const { error } = await deleteS3Source({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`S3 source #${options.id} deleted`)
}

async function listSourceBackupsFn(options: SourceBackupsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid S3 source ID')
    return
  }

  const result = await withSpinner('Fetching source backups...', async () => {
    const { data, error } = await listSourceBackups({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  const backupEntries = result?.backups ?? []

  newline()
  header(`${icons.package} Backups for S3 Source #${options.id} (${backupEntries.length})`)

  if (backupEntries.length === 0) {
    info('No backups found for this S3 source')
    newline()
    return
  }

  if (result?.last_updated) {
    info(`Index last updated: ${formatRelativeTime(result.last_updated)}`)
  }

  const columns: TableColumn<SourceBackupEntry>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Backup ID', key: 'backup_id', width: 12 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'backup_type' },
    { header: 'Size', accessor: (b) => formatBytes(b.size_bytes) },
    { header: 'Created', accessor: (b) => formatRelativeTime(b.created_at), color: (v) => colors.muted(v) },
  ]

  printTable(backupEntries, columns, { style: 'minimal' })
  newline()
}

async function runSourceBackup(options: RunSourceBackupOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid S3 source ID')
    return
  }

  const backup = await withSpinner('Triggering backup...', async () => {
    const { data, error } = await runBackupForSource({
      client,
      path: { id },
      body: {
        backup_type: 'full',
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Backup started for S3 source #${options.id}`)
  info(`Backup ID: ${backup.backup_id}`)
  info(`State: ${backup.state}`)
}

async function runServiceBackup(options: RunServiceBackupOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid external service ID')
    return
  }

  const s3SourceId = parseInt(options.s3SourceId, 10)
  if (isNaN(s3SourceId)) {
    warning('Invalid S3 source ID')
    return
  }

  const backup = await withSpinner('Triggering external service backup...', async () => {
    const { data, error } = await runExternalServiceBackup({
      client,
      path: { id },
      body: {
        s3_source_id: s3SourceId,
        backup_type: options.type || null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Backup started for external service #${options.id}`)
  info(`Backup ID: ${backup.backup_id}`)
  info(`State: ${backup.state}`)
  info(`S3 Location: ${backup.s3_location}`)
}

// Backup commands

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

async function deleteBackup(options: DeleteBackupOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.force && !options.yes) {
    const confirmed = await promptConfirm({
      message: `Permanently delete backup ${options.id} from object storage?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting backup...', async () => {
    const { error } = await client.delete({
      url: '/backups/{id}',
      path: { id: options.id },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Backup ${options.id} deleted`)
}

async function cleanupBackups(options: CleanupBackupsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const scheduleId = options.scheduleId ? Number.parseInt(options.scheduleId, 10) : undefined
  if (options.scheduleId && (!Number.isInteger(scheduleId) || scheduleId! <= 0)) {
    throw new Error('--schedule-id must be a positive integer')
  }

  const preview = await withSpinner('Previewing expired backups...', async () => {
    const { data, error } = await client.post<RetentionCleanupReport>({
      url: '/backups/cleanup',
      query: { dry_run: true, schedule_id: scheduleId },
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error('Cleanup preview returned no report')
    return data
  })

  if (options.dryRun) {
    if (options.json) {
      json(preview)
      return
    }
    info(`Dry run: ${preview.expired} backup${preview.expired === 1 ? '' : 's'} would be deleted`)
    for (const backupId of preview.candidate_backup_ids) info(backupId)
    if (preview.candidate_backup_ids_truncated) warning('More candidates remain beyond this 100-backup batch')
    return
  }

  if (preview.candidate_backup_ids.length === 0) {
    info('No expired backups found')
    return
  }

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: scheduleId
        ? `Delete ${preview.expired} backup(s) expired by schedule ${scheduleId}'s retention policy?`
        : `Delete ${preview.expired} backup(s) older than their schedule retention periods?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const report: RetentionCleanupReport = {
    ...preview,
    dry_run: false,
    expired: 0,
    deleted: 0,
    failed: 0,
    failures: [],
    candidate_backup_ids: [],
    candidate_backup_ids_truncated: false,
  }
  let batch = preview
  while (batch.candidate_backup_ids.length > 0) {
    const current = batch
    const result = await withSpinner(
      `Deleting ${current.candidate_backup_ids.length} previewed backup(s)...`,
      async () => {
        const { data, error } = await client.post<RetentionCleanupReport>({
          url: '/backups/cleanup',
          query: { schedule_id: scheduleId },
          body: { expected_backup_ids: current.candidate_backup_ids },
        })
        if (error) throw new Error(getErrorMessage(error))
        if (!data) throw new Error('Cleanup returned no report')
        return data
      },
    )
    report.expired += result.expired
    report.deleted += result.deleted
    report.failed += result.failed
    report.failures.push(...result.failures)
    const remainingSampleSlots = Math.max(0, 100 - report.candidate_backup_ids.length)
    report.candidate_backup_ids.push(...result.candidate_backup_ids.slice(0, remainingSampleSlots))
    report.candidate_backup_ids_truncated ||=
      result.candidate_backup_ids_truncated || result.candidate_backup_ids.length > remainingSampleSlots
    if (!current.candidate_backup_ids_truncated || result.failed > 0) break
    batch = await withSpinner('Previewing the next cleanup batch...', async () => {
      const { data, error } = await client.post<RetentionCleanupReport>({
        url: '/backups/cleanup',
        query: { dry_run: true, schedule_id: scheduleId },
      })
      if (error) throw new Error(getErrorMessage(error))
      if (!data) throw new Error('Cleanup preview returned no report')
      return data
    })
  }

  if (options.json) {
    json(report)
    return
  }
  success(`Cleanup finished: ${report.deleted} deleted, ${report.failed} failed`)
  if (report.failed > 0) {
    for (const failure of report.failures) {
      warning(`${failure.backup_id}: ${failure.reason}`)
    }
  }
}

// Helpers

function formatBytes(bytes?: number | null): string {
  if (bytes === undefined || bytes === null) return '-'
  if (bytes === 0) return '0 B'

  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  const size = bytes / Math.pow(1024, i)

  return `${size.toFixed(1)} ${units[i]}`
}

function maskSecret(value: string): string {
  if (value.length <= 4) return '***'
  return value.slice(0, 4) + '***' + value.slice(-2)
}
