import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getEnvironments,
  createEnvironment,
  deleteEnvironment,
  getEnvironmentVariables,
  createEnvironmentVariable,
  deleteEnvironmentVariable,
  updateEnvironmentVariable,
  getProjectBySlug,
} from '../../api/sdk.gen.js'
import type { EnvironmentResponse, EnvironmentVariableResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, warning, keyValue, info } from '../../ui/output.js'

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
    .option('--preview', 'Set as preview environment')
    .action(createEnvironmentCmd)

  environments
    .command('delete <project> <environment>')
    .alias('rm')
    .description('Delete an environment')
    .option('-f, --force', 'Skip confirmation')
    .action(deleteEnvironmentCmd)

  // Environment variables subcommand
  const vars = environments
    .command('vars <project>')
    .description('Manage environment variables')

  vars
    .command('list')
    .alias('ls')
    .description('List environment variables')
    .option('--show-secrets', 'Show secret values')
    .option('--json', 'Output in JSON format')
    .action((options, cmd) => {
      const project = cmd.parent!.args[0]
      return listEnvVars(project, options)
    })

  vars
    .command('set <key> [value]')
    .description('Set an environment variable')
    .option('-e, --env <envIds>', 'Comma-separated environment IDs')
    .option('--no-preview', 'Exclude from preview environments')
    .action((key, value, options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return setEnvVar(project, key, value, options)
    })

  vars
    .command('unset <varId>')
    .alias('rm')
    .description('Remove an environment variable by ID')
    .action((varId, _options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return unsetEnvVar(project, varId)
    })
}

async function getProjectId(projectSlug: string): Promise<number> {
  const { data, error } = await getProjectBySlug({
    client,
    path: { slug: projectSlug },
  })
  if (error || !data) {
    throw new Error(`Project "${projectSlug}" not found`)
  }
  return data.id
}

async function listEnvironments(project: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const environments = await withSpinner('Fetching environments...', async () => {
    const projectId = await getProjectId(project)
    const { data, error } = await getEnvironments({
      client,
      path: { project_id: projectId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  if (options.json) {
    json(environments)
    return
  }

  newline()
  header(`${icons.folder} Environments for ${project} (${environments.length})`)

  const columns: TableColumn<EnvironmentResponse>[] = [
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Slug', key: 'slug' },
    { header: 'Branch', accessor: (e) => e.branch ?? '-' },
    { header: 'Preview', accessor: (e) => e.is_preview ? 'Yes' : 'No' },
    { header: 'URL', accessor: (e) => e.main_url ?? '-', color: (v) => colors.primary(v) },
  ]

  printTable(environments, columns, { style: 'minimal' })
  newline()
}

async function createEnvironmentCmd(
  project: string,
  options: { name?: string; branch?: string; preview?: boolean }
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

  await setupClient()

  const environment = await withSpinner('Creating environment...', async () => {
    const projectId = await getProjectId(project)
    const { data, error } = await createEnvironment({
      client,
      path: { project_id: projectId },
      body: {
        name,
        branch,
        set_as_preview: options.preview,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  newline()
  success(`Environment "${name}" created`)
  if (environment?.main_url) {
    info(`URL: ${colors.primary(environment.main_url)}`)
  }
}

async function deleteEnvironmentCmd(
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

  await setupClient()

  await withSpinner('Deleting environment...', async () => {
    const projectId = await getProjectId(project)
    // Environment can be specified by ID or slug, cast to number for TypeScript
    const { error } = await deleteEnvironment({
      client,
      path: { project_id: projectId, env_id: environment as unknown as number },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Environment "${environment}" deleted`)
}

async function listEnvVars(
  project: string,
  options: { showSecrets?: boolean; json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const vars = await withSpinner('Fetching environment variables...', async () => {
    const projectId = await getProjectId(project)
    const { data, error } = await getEnvironmentVariables({
      client,
      path: { project_id: projectId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  if (options.json) {
    json(vars)
    return
  }

  newline()
  header(`${icons.key} Environment Variables (${vars.length})`)

  for (const v of vars) {
    const displayValue = options.showSecrets ? v.value : colors.muted('********')
    const envNames = v.environments.map(e => e.name).join(', ')
    keyValue(`${v.key} (ID: ${v.id})`, displayValue)
    console.log(`    ${colors.muted(`Environments: ${envNames}`)}`)
  }

  newline()
}

async function setEnvVar(
  project: string,
  key: string,
  value: string | undefined,
  options: { env?: string; preview?: boolean }
): Promise<void> {
  await requireAuth()

  const actualValue = value ?? await promptText({
    message: `Value for ${key}`,
    required: true,
  })

  await setupClient()

  await withSpinner(`Setting ${key}...`, async () => {
    const projectId = await getProjectId(project)

    // Get environments to find their IDs
    const { data: envs, error: envsError } = await getEnvironments({
      client,
      path: { project_id: projectId },
    })
    if (envsError) throw new Error(getErrorMessage(envsError))

    // If env option provided, use those IDs; otherwise use all environments
    let environmentIds: number[]
    if (options.env) {
      environmentIds = options.env.split(',').map(id => parseInt(id.trim(), 10))
    } else {
      environmentIds = (envs ?? []).map(e => e.id)
    }

    const { error } = await createEnvironmentVariable({
      client,
      path: { project_id: projectId },
      body: {
        key,
        value: actualValue,
        environment_ids: environmentIds,
        include_in_preview: options.preview !== false,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Set ${key}`)
}

async function unsetEnvVar(project: string, varId: string): Promise<void> {
  await requireAuth()
  await setupClient()

  await withSpinner(`Removing variable...`, async () => {
    const projectId = await getProjectId(project)
    const { error } = await deleteEnvironmentVariable({
      client,
      path: { project_id: projectId, var_id: parseInt(varId, 10) },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Removed variable ${varId}`)
}
