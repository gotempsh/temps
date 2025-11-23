import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm, promptSelect } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, formatDate, formatRelativeTime, keyValue } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface Backup {
  id: number
  name: string
  project_name?: string
  environment?: string
  status: string
  size_bytes?: number
  created_at: string
  completed_at?: string
}

export function registerBackupsCommands(program: Command): void {
  const backups = program
    .command('backups')
    .alias('backup')
    .description('Manage backups')

  backups
    .command('list [project]')
    .alias('ls')
    .description('List backups')
    .option('-e, --environment <env>', 'Filter by environment')
    .option('-n, --limit <number>', 'Limit results', '20')
    .option('--json', 'Output in JSON format')
    .action(listBackups)

  backups
    .command('create <project>')
    .description('Create a backup')
    .option('-e, --environment <env>', 'Environment', 'production')
    .option('-n, --name <name>', 'Backup name')
    .option('--no-wait', 'Do not wait for backup to complete')
    .action(createBackup)

  backups
    .command('restore <backup-id>')
    .description('Restore from a backup')
    .option('-e, --environment <env>', 'Target environment')
    .option('--no-confirm', 'Skip confirmation')
    .action(restoreBackup)

  backups
    .command('delete <backup-id>')
    .alias('rm')
    .description('Delete a backup')
    .option('-f, --force', 'Skip confirmation')
    .action(deleteBackup)

  backups
    .command('download <backup-id>')
    .description('Download a backup')
    .option('-o, --output <path>', 'Output path')
    .action(downloadBackup)
}

async function listBackups(
  project: string | undefined,
  options: { environment?: string; limit: string; json?: boolean }
): Promise<void> {
  await requireAuth()
  const client = getClient()

  const backups = await withSpinner('Fetching backups...', async () => {
    const endpoint = project ? '/api/projects/{project}/backups' : '/api/backups'
    const response = await client.get(endpoint as '/api/backups', {
      params: {
        path: project ? { project } : undefined,
        query: {
          environment: options.environment,
          limit: parseInt(options.limit, 10),
        },
      } as never,
    })
    return (response.data ?? []) as Backup[]
  })

  if (options.json) {
    json(backups)
    return
  }

  newline()
  const title = project
    ? `${icons.package} Backups for ${project} (${backups.length})`
    : `${icons.package} Recent Backups (${backups.length})`
  header(title)

  const columns: TableColumn<Backup>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', accessor: (b) => b.name || `backup-${b.id}`, color: (v) => colors.bold(v) },
    ...(project ? [] : [{ header: 'Project', accessor: (b: Backup) => b.project_name ?? '-' } as TableColumn<Backup>]),
    { header: 'Environment', accessor: (b) => b.environment ?? 'production' },
    { header: 'Status', accessor: (b) => b.status, color: (v) => statusBadge(v) },
    { header: 'Size', accessor: (b) => formatBytes(b.size_bytes) },
    { header: 'Created', accessor: (b) => formatRelativeTime(b.created_at), color: (v) => colors.muted(v) },
  ]

  printTable(backups, columns, { style: 'minimal' })
  newline()
}

async function createBackup(
  project: string,
  options: { environment: string; name?: string; wait?: boolean }
): Promise<void> {
  await requireAuth()
  const client = getClient()

  info(`Creating backup for ${colors.bold(project)} (${options.environment})`)

  const backup = await withSpinner('Initiating backup...', async () => {
    const response = await client.post('/api/backups' as never, {
      body: {
        project_name: project,
        environment: options.environment,
        name: options.name,
      },
    })
    return response.data as Backup
  })

  success(`Backup #${backup.id} created`)

  if (options.wait !== false) {
    await waitForBackup(client, backup.id)
  } else {
    info(`Check status with: temps backups list ${project}`)
  }
}

async function waitForBackup(client: ReturnType<typeof getClient>, backupId: number): Promise<void> {
  const spinner = await import('../../ui/spinner.js')
  spinner.startSpinner('Waiting for backup to complete...')

  // eslint-disable-next-line no-constant-condition
  while (true) {
    const response = await client.get('/api/backups/{id}' as never, {
      params: { path: { id: backupId } },
    })

    const backup = response.data as Backup

    if (backup.status === 'completed' || backup.status === 'success') {
      spinner.succeedSpinner(`Backup completed (${formatBytes(backup.size_bytes)})`)
      return
    }

    if (backup.status === 'failed' || backup.status === 'error') {
      spinner.failSpinner('Backup failed')
      return
    }

    spinner.updateSpinner(`Backup in progress... (${backup.status})`)
    await new Promise((resolve) => setTimeout(resolve, 2000))
  }
}

async function restoreBackup(
  backupId: string,
  options: { environment?: string; confirm?: boolean }
): Promise<void> {
  await requireAuth()

  if (options.confirm !== false) {
    warning('This will overwrite existing data in the target environment!')
    const confirmed = await promptConfirm({
      message: `Restore from backup #${backupId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const client = getClient()

  await withSpinner('Restoring backup...', async () => {
    await client.post('/api/backups/{id}/restore' as never, {
      params: { path: { id: backupId } },
      body: { environment: options.environment },
    })
  })

  success('Restore completed')
}

async function deleteBackup(backupId: string, options: { force?: boolean }): Promise<void> {
  await requireAuth()

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Delete backup #${backupId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const client = getClient()

  await withSpinner('Deleting backup...', async () => {
    await client.delete('/api/backups/{id}' as never, {
      params: { path: { id: backupId } },
    })
  })

  success('Backup deleted')
}

async function downloadBackup(backupId: string, options: { output?: string }): Promise<void> {
  await requireAuth()
  const client = getClient()

  const outputPath = options.output ?? `backup-${backupId}.tar.gz`

  await withSpinner(`Downloading backup to ${outputPath}...`, async () => {
    const response = await client.get('/api/backups/{id}/download' as never, {
      params: { path: { id: backupId } },
      parseAs: 'blob',
    })

    if (response.data) {
      const buffer = Buffer.from(await (response.data as Blob).arrayBuffer())
      await Bun.write(outputPath, buffer)
    }
  })

  success(`Backup downloaded to ${outputPath}`)
}

function formatBytes(bytes?: number): string {
  if (bytes === undefined || bytes === null) return '-'
  if (bytes === 0) return '0 B'

  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  const size = bytes / Math.pow(1024, i)

  return `${size.toFixed(1)} ${units[i]}`
}
