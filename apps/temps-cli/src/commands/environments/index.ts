import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm, promptSelect } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, warning, keyValue, info } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface Environment {
  id: number
  name: string
  project_name?: string
  status: string
  url?: string
  branch?: string
  auto_deploy: boolean
  created_at: string
}

interface EnvVar {
  key: string
  value: string
  is_secret: boolean
}

export function registerEnvironmentsCommands(program: Command): void {
  const environments = program
    .command('environments')
    .alias('envs')
    .alias('env')
    .description('Manage environments')

  environments
    .command('list <project>')
    .alias('ls')
    .description('List environments for a project')
    .option('--json', 'Output in JSON format')
    .action(listEnvironments)

  environments
    .command('create <project>')
    .description('Create a new environment')
    .option('-n, --name <name>', 'Environment name')
    .option('-b, --branch <branch>', 'Git branch')
    .option('--no-auto-deploy', 'Disable auto-deploy')
    .action(createEnvironment)

  environments
    .command('delete <project> <environment>')
    .alias('rm')
    .description('Delete an environment')
    .option('-f, --force', 'Skip confirmation')
    .action(deleteEnvironment)

  // Environment variables subcommand
  const vars = environments
    .command('vars <project> <environment>')
    .description('Manage environment variables')

  vars
    .command('list')
    .alias('ls')
    .description('List environment variables')
    .option('--show-secrets', 'Show secret values')
    .option('--json', 'Output in JSON format')
    .action((options, cmd) => {
      const [project, environment] = cmd.parent!.args
      return listEnvVars(project, environment, options)
    })

  vars
    .command('set <key> [value]')
    .description('Set an environment variable')
    .option('-s, --secret', 'Mark as secret')
    .action((key, value, options, cmd) => {
      const [project, environment] = cmd.parent!.parent!.args
      return setEnvVar(project, environment, key, value, options)
    })

  vars
    .command('unset <key>')
    .alias('rm')
    .description('Remove an environment variable')
    .action((key, _options, cmd) => {
      const [project, environment] = cmd.parent!.parent!.args
      return unsetEnvVar(project, environment, key)
    })
}

async function listEnvironments(project: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  const client = getClient()

  const environments = await withSpinner('Fetching environments...', async () => {
    const response = await client.get('/api/projects/{project}/environments' as never, {
      params: { path: { project } },
    })
    return (response.data ?? []) as Environment[]
  })

  if (options.json) {
    json(environments)
    return
  }

  newline()
  header(`${icons.folder} Environments for ${project} (${environments.length})`)

  const columns: TableColumn<Environment>[] = [
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Status', accessor: (e) => e.status, color: (v) => statusBadge(v) },
    { header: 'Branch', accessor: (e) => e.branch ?? '-' },
    { header: 'URL', accessor: (e) => e.url ?? '-', color: (v) => colors.primary(v) },
    { header: 'Auto-deploy', accessor: (e) => e.auto_deploy ? 'Yes' : 'No' },
  ]

  printTable(environments, columns, { style: 'minimal' })
  newline()
}

async function createEnvironment(
  project: string,
  options: { name?: string; branch?: string; autoDeploy?: boolean }
): Promise<void> {
  await requireAuth()

  const name = options.name ?? await promptText({
    message: 'Environment name',
    required: true,
    validate: (v) => /^[a-z0-9-]+$/.test(v) || 'Use lowercase letters, numbers, and hyphens only',
  })

  const branch = options.branch ?? await promptText({
    message: 'Git branch',
    default: name === 'production' ? 'main' : name,
  })

  const client = getClient()

  const environment = await withSpinner('Creating environment...', async () => {
    const response = await client.post('/api/projects/{project}/environments' as never, {
      params: { path: { project } },
      body: {
        name,
        branch,
        auto_deploy: options.autoDeploy !== false,
      },
    })
    return response.data as Environment
  })

  newline()
  success(`Environment "${name}" created`)
  if (environment.url) {
    info(`URL: ${colors.primary(environment.url)}`)
  }
}

async function deleteEnvironment(
  project: string,
  environment: string,
  options: { force?: boolean }
): Promise<void> {
  await requireAuth()

  if (environment === 'production') {
    warning('Cannot delete production environment')
    return
  }

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Delete environment "${environment}" from ${project}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const client = getClient()

  await withSpinner('Deleting environment...', async () => {
    await client.delete('/api/projects/{project}/environments/{environment}' as never, {
      params: { path: { project, environment } },
    })
  })

  success(`Environment "${environment}" deleted`)
}

async function listEnvVars(
  project: string,
  environment: string,
  options: { showSecrets?: boolean; json?: boolean }
): Promise<void> {
  await requireAuth()
  const client = getClient()

  const vars = await withSpinner('Fetching environment variables...', async () => {
    const response = await client.get('/api/projects/{project}/environments/{environment}/vars' as never, {
      params: { path: { project, environment } },
    })
    return (response.data ?? []) as EnvVar[]
  })

  if (options.json) {
    json(vars)
    return
  }

  newline()
  header(`${icons.key} Environment Variables (${vars.length})`)

  for (const v of vars) {
    const displayValue = v.is_secret && !options.showSecrets
      ? colors.muted('********')
      : v.value
    keyValue(v.key, displayValue)
  }

  newline()
}

async function setEnvVar(
  project: string,
  environment: string,
  key: string,
  value: string | undefined,
  options: { secret?: boolean }
): Promise<void> {
  await requireAuth()

  const actualValue = value ?? await promptText({
    message: `Value for ${key}`,
    required: true,
  })

  const client = getClient()

  await withSpinner(`Setting ${key}...`, async () => {
    await client.put('/api/projects/{project}/environments/{environment}/vars/{key}' as never, {
      params: { path: { project, environment, key } },
      body: {
        value: actualValue,
        is_secret: options.secret ?? false,
      },
    })
  })

  success(`Set ${key}`)
}

async function unsetEnvVar(project: string, environment: string, key: string): Promise<void> {
  await requireAuth()
  const client = getClient()

  await withSpinner(`Removing ${key}...`, async () => {
    await client.delete('/api/projects/{project}/environments/{environment}/vars/{key}' as never, {
      params: { path: { project, environment, key } },
    })
  })

  success(`Removed ${key}`)
}
