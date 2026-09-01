// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listDsns,
  createDsn,
  getOrCreateDsn,
  regenerateDsn,
  revokeDsn,
} from '../../api/sdk.gen.js'
import type { ProjectDsnResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm, promptText } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface ListOptions {
  projectId: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  name?: string
  environmentId?: string
  deploymentId?: string
  baseUrl?: string
  yes?: boolean
}

interface GetOrCreateOptions {
  projectId: string
  environmentId?: string
  deploymentId?: string
  baseUrl?: string
  json?: boolean
}

interface RegenerateOptions {
  projectId: string
  dsnId: string
  baseUrl?: string
  force?: boolean
  yes?: boolean
}

interface RevokeOptions {
  projectId: string
  dsnId: string
  force?: boolean
  yes?: boolean
}

export function registerDsnCommands(program: Command): void {
  const dsn = program
    .command('dsn')
    .description('Manage Data Source Names (DSNs) for error tracking and analytics')

  dsn
    .command('list')
    .alias('ls')
    .description('List all DSNs for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listDsnsAction)

  dsn
    .command('create')
    .alias('add')
    .description('Create a new DSN for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-n, --name <name>', 'DSN name')
    .option('--environment-id <id>', 'Environment ID')
    .option('--deployment-id <id>', 'Deployment ID')
    .option('--base-url <url>', 'Base URL for the DSN')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createDsnAction)

  dsn
    .command('get-or-create')
    .description('Get an existing DSN or create one if none exists')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--environment-id <id>', 'Environment ID')
    .option('--deployment-id <id>', 'Deployment ID')
    .option('--base-url <url>', 'Base URL for the DSN')
    .option('--json', 'Output in JSON format')
    .action(getOrCreateDsnAction)

  dsn
    .command('regenerate')
    .description('Regenerate DSN keys (rotate keys)')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--dsn-id <id>', 'DSN ID')
    .option('--base-url <url>', 'New base URL for the DSN')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(regenerateDsnAction)

  dsn
    .command('revoke')
    .description('Revoke (deactivate) a DSN')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--dsn-id <id>', 'DSN ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(revokeDsnAction)
}

async function listDsnsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const dsns = await withSpinner('Fetching DSNs...', async () => {
    const { data, error } = await listDsns({
      client,
      path: { project_id: projectId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(dsns)
    return
  }

  newline()
  header(`${icons.info} DSNs (${dsns.length})`)

  if (dsns.length === 0) {
    info('No DSNs found for this project')
    info(`Run: temps dsn create --project-id ${options.projectId} --name my-dsn -y`)
    newline()
    return
  }

  const columns: TableColumn<ProjectDsnResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Status', accessor: (d) => d.is_active ? 'active' : 'revoked', color: (v) => statusBadge(v === 'active' ? 'active' : 'inactive') },
    { header: 'Public Key', accessor: (d) => truncateKey(d.public_key) },
    { header: 'Events', accessor: (d) => String(d.event_count) },
    { header: 'Created', accessor: (d) => new Date(d.created_at).toLocaleDateString() },
  ]

  printTable(dsns, columns, { style: 'minimal' })
  newline()
}

async function createDsnAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let name: string | undefined = options.name
  let environmentId: number | undefined
  let deploymentId: number | undefined

  if (options.environmentId) {
    environmentId = parseInt(options.environmentId, 10)
    if (isNaN(environmentId)) {
      warning('Invalid environment ID')
      return
    }
  }

  if (options.deploymentId) {
    deploymentId = parseInt(options.deploymentId, 10)
    if (isNaN(deploymentId)) {
      warning('Invalid deployment ID')
      return
    }
  }

  if (!options.yes && !name) {
    name = await promptText({
      message: 'DSN name',
      default: 'default',
      required: true,
    })
  }

  const result = await withSpinner('Creating DSN...', async () => {
    const { data, error } = await createDsn({
      client,
      path: { project_id: projectId },
      body: {
        name: name ?? null,
        environment_id: environmentId ?? null,
        deployment_id: deploymentId ?? null,
        base_url: options.baseUrl ?? null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (result) {
    newline()
    success('DSN created successfully')
    newline()
    displayDsnDetails(result)
  }
}

async function getOrCreateDsnAction(options: GetOrCreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let environmentId: number | undefined
  let deploymentId: number | undefined

  if (options.environmentId) {
    environmentId = parseInt(options.environmentId, 10)
    if (isNaN(environmentId)) {
      warning('Invalid environment ID')
      return
    }
  }

  if (options.deploymentId) {
    deploymentId = parseInt(options.deploymentId, 10)
    if (isNaN(deploymentId)) {
      warning('Invalid deployment ID')
      return
    }
  }

  const result = await withSpinner('Retrieving DSN...', async () => {
    const { data, error } = await getOrCreateDsn({
      client,
      path: { project_id: projectId },
      body: {
        environment_id: environmentId ?? null,
        deployment_id: deploymentId ?? null,
        base_url: options.baseUrl ?? null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (result) {
    if (options.json) {
      json(result)
      return
    }

    newline()
    success('DSN retrieved')
    newline()
    displayDsnDetails(result)
  }
}

async function regenerateDsnAction(options: RegenerateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const dsnId = parseInt(options.dsnId, 10)
  if (isNaN(dsnId)) {
    warning('Invalid DSN ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('Regenerating keys will invalidate the current DSN string.')
    warning('Any clients using the old DSN will stop working.')
    const confirmed = await promptConfirm({
      message: `Regenerate keys for DSN ${dsnId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const result = await withSpinner('Regenerating DSN keys...', async () => {
    const { data, error } = await regenerateDsn({
      client,
      path: { project_id: projectId, dsn_id: dsnId },
      body: {
        base_url: options.baseUrl ?? null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (result) {
    newline()
    success('DSN keys regenerated')
    newline()
    displayDsnDetails(result)
    warning('Update your application with the new DSN string.')
  }
}

async function revokeDsnAction(options: RevokeOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const dsnId = parseInt(options.dsnId, 10)
  if (isNaN(dsnId)) {
    warning('Invalid DSN ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('This will permanently revoke the DSN.')
    warning('Any clients using this DSN will stop sending data.')
    const confirmed = await promptConfirm({
      message: `Revoke DSN ${dsnId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Revoking DSN...', async () => {
    const { error } = await revokeDsn({
      client,
      path: { project_id: projectId, dsn_id: dsnId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('DSN revoked')
  info('The DSN is now inactive and will no longer accept events.')
}

function displayDsnDetails(dsn: ProjectDsnResponse): void {
  header(`${icons.info} ${dsn.name}`)
  keyValue('ID', dsn.id)
  keyValue('DSN', colors.bold(dsn.dsn))
  keyValue('Public Key', dsn.public_key)
  keyValue('Status', dsn.is_active ? colors.success('Active') : colors.muted('Revoked'))
  keyValue('Events', dsn.event_count)
  if (dsn.environment_id) {
    keyValue('Environment', dsn.environment_id)
  }
  if (dsn.deployment_id) {
    keyValue('Deployment', dsn.deployment_id)
  }
  keyValue('Created', new Date(dsn.created_at).toLocaleString())
  newline()
}

export function truncateKey(key: string): string {
  if (key.length <= 12) return key
  return `${key.substring(0, 8)}...${key.substring(key.length - 4)}`
}
