import type { Command } from 'commander'
import * as fs from 'node:fs'
import * as path from 'node:path'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getEnvironments,
  getEnvironment,
  createEnvironment,
  deleteEnvironment,
  getEnvironmentVariables,
  createEnvironmentVariable,
  deleteEnvironmentVariable,
  updateEnvironmentVariable,
  updateEnvironmentSettings,
  getProjectBySlug,
} from '../../api/sdk.gen.js'
import type { EnvironmentResponse, EnvironmentVariableResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm, promptSelect } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, warning, keyValue, info, error as errorOutput } from '../../ui/output.js'

export function registerEnvironmentsCommands(program: Command): void {
  const environments = program
    .command('environments')
    .alias('envs')
    .alias('env')
    .description('Manage environments and environment variables')

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
    .option('-e, --environment <name>', 'Filter by environment name')
    .option('--show-values', 'Show actual values (hidden by default)')
    .option('--json', 'Output in JSON format')
    .action((options, cmd) => {
      const project = cmd.parent!.args[0]
      return listEnvVars(project, options)
    })

  vars
    .command('get <key>')
    .description('Get a specific environment variable')
    .option('-e, --environment <name>', 'Specify environment (if variable exists in multiple)')
    .action((key, options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return getEnvVar(project, key, options)
    })

  vars
    .command('set <key> [value]')
    .description('Set an environment variable')
    .option('-e, --environments <names>', 'Comma-separated environment names (interactive if not provided)')
    .option('--no-preview', 'Exclude from preview environments')
    .option('--update', 'Update existing variable instead of creating new')
    .action((key, value, options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return setEnvVar(project, key, value, options)
    })

  vars
    .command('delete <key>')
    .alias('rm')
    .alias('unset')
    .description('Delete an environment variable')
    .option('-e, --environment <name>', 'Delete only from specific environment')
    .option('-f, --force', 'Skip confirmation')
    .action((key, options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return deleteEnvVar(project, key, options)
    })

  vars
    .command('import [file]')
    .description('Import environment variables from a .env file')
    .option('-e, --environments <names>', 'Comma-separated environment names')
    .option('--overwrite', 'Overwrite existing variables')
    .action((file, options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return importEnvVars(project, file, options)
    })

  vars
    .command('export')
    .description('Export environment variables to .env format')
    .option('-e, --environment <name>', 'Export from specific environment')
    .option('-o, --output <file>', 'Write to file instead of stdout')
    .action((options, cmd) => {
      const project = cmd.parent!.parent!.args[0]
      return exportEnvVars(project, options)
    })

  // Resources subcommand
  environments
    .command('resources <project> <environment>')
    .description('View or set CPU/memory resources for an environment')
    .option('--cpu <millicores>', 'CPU limit in millicores (e.g., 500 = 0.5 CPU)')
    .option('--memory <mb>', 'Memory limit in MB (e.g., 512)')
    .option('--cpu-request <millicores>', 'CPU request in millicores (guaranteed minimum)')
    .option('--memory-request <mb>', 'Memory request in MB (guaranteed minimum)')
    .option('--json', 'Output in JSON format')
    .action(resourcesCmd)

  // Scale subcommand
  environments
    .command('scale <project> <environment> [replicas]')
    .description('View or set the number of replicas for an environment')
    .option('--json', 'Output in JSON format')
    .action(scaleCmd)
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
    const { error } = await deleteEnvironment({
      client,
      path: { project_id: projectId, env_id: environment as unknown as number },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Environment "${environment}" deleted`)
}

// ============ Environment Variables Commands ============

async function listEnvVars(
  project: string,
  options: { environment?: string; showValues?: boolean; json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const [vars, environments] = await withSpinner('Fetching environment variables...', async () => {
    const projectId = await getProjectId(project)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({
        client,
        path: { project_id: projectId },
      }),
      getEnvironments({
        client,
        path: { project_id: projectId },
      }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return [varsResult.data ?? [], envsResult.data ?? []] as const
  })

  // Filter by environment if specified
  let filteredVars = vars
  if (options.environment) {
    const targetEnv = environments.find(
      e => e.name.toLowerCase() === options.environment!.toLowerCase() ||
           e.slug === options.environment!.toLowerCase()
    )
    if (!targetEnv) {
      errorOutput(`Environment "${options.environment}" not found`)
      info(`Available environments: ${environments.map(e => e.name).join(', ')}`)
      return
    }
    filteredVars = vars.filter(v =>
      v.environments.some(e => e.id === targetEnv.id)
    )
  }

  if (options.json) {
    json(filteredVars)
    return
  }

  newline()
  const title = options.environment
    ? `${icons.key} Environment Variables for ${project} (${options.environment})`
    : `${icons.key} Environment Variables for ${project}`
  header(`${title} (${filteredVars.length})`)

  if (filteredVars.length === 0) {
    info('No environment variables found')
    newline()
    return
  }

  // Group by key for better visualization
  const columns: TableColumn<EnvironmentVariableResponse>[] = [
    { header: 'ID', key: 'id', color: (v) => colors.muted(String(v)) },
    { header: 'Key', key: 'key', color: (v) => colors.bold(v) },
    {
      header: 'Value',
      accessor: (v) => options.showValues ? v.value : '••••••••',
      color: (v) => options.showValues ? colors.primary(v) : colors.muted(v),
    },
    {
      header: 'Environments',
      accessor: (v) => v.environments.map(e => e.name).join(', ') || 'None',
      color: (v) => colors.muted(v),
    },
    {
      header: 'Preview',
      accessor: (v) => v.include_in_preview ? '✓' : '✗',
      color: (v) => v === '✓' ? colors.success(v) : colors.muted(v),
    },
  ]

  printTable(filteredVars, columns, { style: 'minimal' })

  if (!options.showValues) {
    newline()
    info(`Use ${colors.bold('--show-values')} to reveal actual values`)
  }
  newline()
}

async function getEnvVar(
  project: string,
  key: string,
  options: { environment?: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const [vars, environments] = await withSpinner(`Fetching ${key}...`, async () => {
    const projectId = await getProjectId(project)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({
        client,
        path: { project_id: projectId },
      }),
      getEnvironments({
        client,
        path: { project_id: projectId },
      }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return [varsResult.data ?? [], envsResult.data ?? []] as const
  })

  // Find variables with the given key
  const matchingVars = vars.filter(v => v.key === key)

  if (matchingVars.length === 0) {
    errorOutput(`Variable "${key}" not found`)
    return
  }

  // If environment specified, filter to that environment
  if (options.environment) {
    const targetEnv = environments.find(
      e => e.name.toLowerCase() === options.environment!.toLowerCase() ||
           e.slug === options.environment!.toLowerCase()
    )
    if (!targetEnv) {
      errorOutput(`Environment "${options.environment}" not found`)
      return
    }

    const envVar = matchingVars.find(v =>
      v.environments.some(e => e.id === targetEnv.id)
    )
    if (!envVar) {
      errorOutput(`Variable "${key}" not found in environment "${options.environment}"`)
      return
    }

    newline()
    keyValue('Key', envVar.key)
    keyValue('Value', envVar.value)
    keyValue('Environment', targetEnv.name)
    keyValue('Include in Preview', envVar.include_in_preview ? 'Yes' : 'No')
    newline()
    return
  }

  // Show all matching variables
  newline()
  header(`${icons.key} ${key}`)

  for (const v of matchingVars) {
    keyValue('ID', String(v.id))
    keyValue('Value', v.value)
    keyValue('Environments', v.environments.map(e => e.name).join(', ') || 'None')
    keyValue('Include in Preview', v.include_in_preview ? 'Yes' : 'No')
    newline()
  }
}

async function setEnvVar(
  project: string,
  key: string,
  value: string | undefined,
  options: { environments?: string; preview?: boolean; update?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  // Get environments first
  const [existingVars, envs] = await withSpinner('Fetching environments...', async () => {
    const projectId = await getProjectId(project)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({
        client,
        path: { project_id: projectId },
      }),
      getEnvironments({
        client,
        path: { project_id: projectId },
      }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return [varsResult.data ?? [], envsResult.data ?? []] as const
  })

  if (envs.length === 0) {
    errorOutput('No environments found. Create an environment first.')
    return
  }

  // Check if variable already exists
  const existingVar = existingVars.find(v => v.key === key)

  // Get value if not provided
  const actualValue = value ?? await promptText({
    message: `Value for ${key}`,
    required: true,
  })

  // Determine which environments to use
  let environmentIds: number[]
  if (options.environments) {
    // Parse comma-separated environment names
    const envNames = options.environments.split(',').map(n => n.trim().toLowerCase())
    environmentIds = []
    for (const name of envNames) {
      const env = envs.find(e =>
        e.name.toLowerCase() === name || e.slug === name
      )
      if (!env) {
        errorOutput(`Environment "${name}" not found`)
        info(`Available environments: ${envs.map(e => e.name).join(', ')}`)
        return
      }
      environmentIds.push(env.id)
    }
  } else {
    // Interactive environment selection
    const choices = envs.map(e => ({
      name: `${e.name} ${e.is_preview ? '(preview)' : ''}`,
      value: e.id,
    }))

    // Default to all environments if not updating
    if (existingVar && options.update) {
      environmentIds = existingVar.environments.map(e => e.id)
    } else {
      const selected = await promptSelect({
        message: 'Select environments',
        choices,
        multiple: true,
      }) as number[]
      environmentIds = selected
    }
  }

  const projectId = await getProjectId(project)

  if (existingVar && options.update) {
    // Update existing variable
    await withSpinner(`Updating ${key}...`, async () => {
      const { error } = await updateEnvironmentVariable({
        client,
        path: { project_id: projectId, var_id: existingVar.id },
        body: {
          value: actualValue,
          environment_ids: environmentIds,
          include_in_preview: options.preview !== false,
        },
      })
      if (error) throw new Error(getErrorMessage(error))
    })
    success(`Updated ${key}`)
  } else {
    // Create new variable
    await withSpinner(`Setting ${key}...`, async () => {
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

  info(`Environments: ${envs.filter(e => environmentIds.includes(e.id)).map(e => e.name).join(', ')}`)
}

async function deleteEnvVar(
  project: string,
  key: string,
  options: { environment?: string; force?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const [vars, environments] = await withSpinner('Fetching variables...', async () => {
    const projectId = await getProjectId(project)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({
        client,
        path: { project_id: projectId },
      }),
      getEnvironments({
        client,
        path: { project_id: projectId },
      }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return [varsResult.data ?? [], envsResult.data ?? []] as const
  })

  // Find variables with the given key
  const matchingVars = vars.filter(v => v.key === key)

  if (matchingVars.length === 0) {
    errorOutput(`Variable "${key}" not found`)
    return
  }

  let varToDelete: EnvironmentVariableResponse | undefined

  if (options.environment) {
    // Find variable in specific environment
    const targetEnv = environments.find(
      e => e.name.toLowerCase() === options.environment!.toLowerCase() ||
           e.slug === options.environment!.toLowerCase()
    )
    if (!targetEnv) {
      errorOutput(`Environment "${options.environment}" not found`)
      return
    }

    varToDelete = matchingVars.find(v =>
      v.environments.some(e => e.id === targetEnv.id)
    )
    if (!varToDelete) {
      errorOutput(`Variable "${key}" not found in environment "${options.environment}"`)
      return
    }
  } else if (matchingVars.length === 1) {
    varToDelete = matchingVars[0]
  } else {
    // Multiple variables with same key, ask user which one
    const choices = matchingVars.map(v => ({
      name: `${key} (ID: ${v.id}) - Environments: ${v.environments.map(e => e.name).join(', ')}`,
      value: v.id,
    }))

    const selectedId = await promptSelect({
      message: `Multiple variables found for "${key}". Select which one to delete:`,
      choices,
    }) as number

    varToDelete = matchingVars.find(v => v.id === selectedId)
  }

  if (!varToDelete) {
    errorOutput('No variable selected')
    return
  }

  // Confirm deletion
  if (!options.force) {
    const envNames = varToDelete.environments.map(e => e.name).join(', ')
    const confirmed = await promptConfirm({
      message: `Delete "${key}" from environments: ${envNames}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const projectId = await getProjectId(project)

  await withSpinner(`Deleting ${key}...`, async () => {
    const { error } = await deleteEnvironmentVariable({
      client,
      path: { project_id: projectId, var_id: varToDelete!.id },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Deleted ${key}`)
}

async function importEnvVars(
  project: string,
  file: string | undefined,
  options: { environments?: string; overwrite?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  // Default to .env in current directory
  const filePath = file ?? '.env'
  const absolutePath = path.isAbsolute(filePath) ? filePath : path.resolve(process.cwd(), filePath)

  // Check if file exists
  if (!fs.existsSync(absolutePath)) {
    errorOutput(`File not found: ${absolutePath}`)
    return
  }

  // Parse .env file
  const content = fs.readFileSync(absolutePath, 'utf-8')
  const variables = parseEnvFile(content)

  if (Object.keys(variables).length === 0) {
    warning('No variables found in file')
    return
  }

  info(`Found ${Object.keys(variables).length} variables in ${filePath}`)

  // Get environments
  const [existingVars, envs] = await withSpinner('Fetching environments...', async () => {
    const projectId = await getProjectId(project)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({
        client,
        path: { project_id: projectId },
      }),
      getEnvironments({
        client,
        path: { project_id: projectId },
      }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return [varsResult.data ?? [], envsResult.data ?? []] as const
  })

  if (envs.length === 0) {
    errorOutput('No environments found. Create an environment first.')
    return
  }

  // Determine which environments to use
  let environmentIds: number[]
  if (options.environments) {
    const envNames = options.environments.split(',').map(n => n.trim().toLowerCase())
    environmentIds = []
    for (const name of envNames) {
      const env = envs.find(e =>
        e.name.toLowerCase() === name || e.slug === name
      )
      if (!env) {
        errorOutput(`Environment "${name}" not found`)
        return
      }
      environmentIds.push(env.id)
    }
  } else {
    // Interactive selection
    const choices = envs.map(e => ({
      name: `${e.name} ${e.is_preview ? '(preview)' : ''}`,
      value: e.id,
    }))

    const selected = await promptSelect({
      message: 'Select environments to import into',
      choices,
      multiple: true,
    }) as number[]
    environmentIds = selected
  }

  const projectId = await getProjectId(project)
  let created = 0
  let updated = 0
  let skipped = 0

  for (const [key, value] of Object.entries(variables)) {
    const existing = existingVars.find(v => v.key === key)

    if (existing) {
      if (options.overwrite) {
        try {
          await updateEnvironmentVariable({
            client,
            path: { project_id: projectId, var_id: existing.id },
            body: {
              value,
              environment_ids: environmentIds,
              include_in_preview: true,
            },
          })
          updated++
        } catch (e) {
          warning(`Failed to update ${key}: ${getErrorMessage(e)}`)
        }
      } else {
        skipped++
      }
    } else {
      try {
        await createEnvironmentVariable({
          client,
          path: { project_id: projectId },
          body: {
            key,
            value,
            environment_ids: environmentIds,
            include_in_preview: true,
          },
        })
        created++
      } catch (e) {
        warning(`Failed to create ${key}: ${getErrorMessage(e)}`)
      }
    }
  }

  newline()
  success(`Import complete: ${created} created, ${updated} updated, ${skipped} skipped`)
  if (skipped > 0 && !options.overwrite) {
    info(`Use ${colors.bold('--overwrite')} to update existing variables`)
  }
}

async function exportEnvVars(
  project: string,
  options: { environment?: string; output?: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const [vars, environments] = await withSpinner('Fetching environment variables...', async () => {
    const projectId = await getProjectId(project)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({
        client,
        path: { project_id: projectId },
      }),
      getEnvironments({
        client,
        path: { project_id: projectId },
      }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return [varsResult.data ?? [], envsResult.data ?? []] as const
  })

  let filteredVars = vars

  if (options.environment) {
    const targetEnv = environments.find(
      e => e.name.toLowerCase() === options.environment!.toLowerCase() ||
           e.slug === options.environment!.toLowerCase()
    )
    if (!targetEnv) {
      errorOutput(`Environment "${options.environment}" not found`)
      info(`Available environments: ${environments.map(e => e.name).join(', ')}`)
      return
    }
    filteredVars = vars.filter(v =>
      v.environments.some(e => e.id === targetEnv.id)
    )
  }

  if (filteredVars.length === 0) {
    warning('No environment variables to export')
    return
  }

  // Generate .env content
  const envContent = filteredVars
    .map(v => {
      const escapedValue = v.value.includes('\n') || v.value.includes('"')
        ? `"${v.value.replace(/"/g, '\\"').replace(/\n/g, '\\n')}"`
        : v.value.includes(' ') || v.value.includes('#')
          ? `"${v.value}"`
          : v.value
      return `${v.key}=${escapedValue}`
    })
    .join('\n')

  if (options.output) {
    const outputPath = path.isAbsolute(options.output)
      ? options.output
      : path.resolve(process.cwd(), options.output)
    fs.writeFileSync(outputPath, envContent + '\n')
    success(`Exported ${filteredVars.length} variables to ${options.output}`)
  } else {
    // Output to stdout
    console.log(envContent)
  }
}

// ============ Resources Command ============

interface ResourcesOptions {
  cpu?: string
  memory?: string
  cpuRequest?: string
  memoryRequest?: string
  json?: boolean
}

async function resourcesCmd(
  project: string,
  environment: string,
  options: ResourcesOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await getProjectId(project)

  // Find environment by slug
  const envs = await withSpinner('Fetching environments...', async () => {
    const { data, error } = await getEnvironments({
      client,
      path: { project_id: projectId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  const targetEnv = envs.find(
    e => e.slug === environment || e.name.toLowerCase() === environment.toLowerCase()
  )

  if (!targetEnv) {
    errorOutput(`Environment "${environment}" not found`)
    info(`Available environments: ${envs.map(e => e.slug).join(', ')}`)
    return
  }

  // Check if any resource options are provided
  const hasResourceOptions = options.cpu || options.memory || options.cpuRequest || options.memoryRequest

  if (hasResourceOptions) {
    // Update resources
    const updateBody: {
      cpu_limit?: number | null
      cpu_request?: number | null
      memory_limit?: number | null
      memory_request?: number | null
    } = {}

    // Parse CPU limit
    let cpuLimit: number | undefined
    if (options.cpu) {
      cpuLimit = parseInt(options.cpu, 10)
      if (isNaN(cpuLimit) || cpuLimit <= 0) {
        errorOutput('CPU must be a positive number (millicores)')
        return
      }
      updateBody.cpu_limit = cpuLimit
    }

    // Parse memory limit
    let memoryLimit: number | undefined
    if (options.memory) {
      memoryLimit = parseInt(options.memory, 10)
      if (isNaN(memoryLimit) || memoryLimit <= 0) {
        errorOutput('Memory must be a positive number (MB)')
        return
      }
      updateBody.memory_limit = memoryLimit
    }

    // Parse CPU request (or default to limit)
    if (options.cpuRequest) {
      const cpuRequest = parseInt(options.cpuRequest, 10)
      if (isNaN(cpuRequest) || cpuRequest <= 0) {
        errorOutput('CPU request must be a positive number (millicores)')
        return
      }
      updateBody.cpu_request = cpuRequest
    } else if (cpuLimit !== undefined) {
      // Default request to same as limit when setting limit
      updateBody.cpu_request = cpuLimit
    }

    // Parse memory request (or default to limit)
    if (options.memoryRequest) {
      const memoryRequest = parseInt(options.memoryRequest, 10)
      if (isNaN(memoryRequest) || memoryRequest <= 0) {
        errorOutput('Memory request must be a positive number (MB)')
        return
      }
      updateBody.memory_request = memoryRequest
    } else if (memoryLimit !== undefined) {
      // Default request to same as limit when setting limit
      updateBody.memory_request = memoryLimit
    }

    const updatedEnv = await withSpinner('Updating resources...', async () => {
      const { data, error } = await updateEnvironmentSettings({
        client,
        path: { project_id: projectId, env_id: targetEnv.id },
        body: updateBody,
      })
      if (error) throw new Error(getErrorMessage(error))
      return data
    })

    if (options.json) {
      const config = updatedEnv?.deployment_config
      json({
        environment: updatedEnv?.slug,
        cpu_limit: config?.cpuLimit,
        cpu_request: config?.cpuRequest,
        memory_limit: config?.memoryLimit,
        memory_request: config?.memoryRequest,
      })
      return
    }

    newline()
    success(`Resources updated for ${project}/${environment}`)
    newline()
    displayResources(updatedEnv)
  } else {
    // Display current resources
    if (options.json) {
      const config = targetEnv.deployment_config
      json({
        environment: targetEnv.slug,
        cpu_limit: config?.cpuLimit,
        cpu_request: config?.cpuRequest,
        memory_limit: config?.memoryLimit,
        memory_request: config?.memoryRequest,
      })
      return
    }

    newline()
    header(`${icons.folder} Resources for ${project}/${environment}`)
    newline()
    displayResources(targetEnv)
  }
}

function displayResources(env: EnvironmentResponse | null | undefined): void {
  if (!env) return

  const config = env.deployment_config

  const formatCpu = (millicores: number | null | undefined): string => {
    if (millicores == null) return colors.muted('not set')
    const cores = millicores / 1000
    return `${millicores}m (${cores} CPU)`
  }

  const formatMemory = (mb: number | null | undefined): string => {
    if (mb == null) return colors.muted('not set')
    if (mb >= 1024) {
      return `${mb}MB (${(mb / 1024).toFixed(1)}GB)`
    }
    return `${mb}MB`
  }

  keyValue('CPU Limit', formatCpu(config?.cpuLimit))
  keyValue('CPU Request', formatCpu(config?.cpuRequest))
  keyValue('Memory Limit', formatMemory(config?.memoryLimit))
  keyValue('Memory Request', formatMemory(config?.memoryRequest))
  newline()

  info(`${colors.bold('Limits')} = maximum resources the container can use`)
  info(`${colors.bold('Requests')} = guaranteed minimum resources`)
  newline()
  info(`Example: ${colors.muted('temps env resources my-project production --cpu 1000 --memory 512')}`)
}

// ============ Scale Command ============

async function scaleCmd(
  project: string,
  environment: string,
  replicas: string | undefined,
  options: { json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await getProjectId(project)

  // Find environment by slug
  const envs = await withSpinner('Fetching environments...', async () => {
    const { data, error } = await getEnvironments({
      client,
      path: { project_id: projectId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  const targetEnv = envs.find(
    e => e.slug === environment || e.name.toLowerCase() === environment.toLowerCase()
  )

  if (!targetEnv) {
    errorOutput(`Environment "${environment}" not found`)
    info(`Available environments: ${envs.map(e => e.slug).join(', ')}`)
    return
  }

  if (replicas !== undefined) {
    // Set replicas
    const replicaCount = parseInt(replicas, 10)
    if (isNaN(replicaCount) || replicaCount < 0) {
      errorOutput('Replicas must be a non-negative number')
      return
    }

    if (replicaCount > 10) {
      warning(`Setting ${replicaCount} replicas. This may consume significant resources.`)
    }

    const updatedEnv = await withSpinner(`Scaling to ${replicaCount} replica${replicaCount !== 1 ? 's' : ''}...`, async () => {
      const { data, error } = await updateEnvironmentSettings({
        client,
        path: { project_id: projectId, env_id: targetEnv.id },
        body: { replicas: replicaCount },
      })
      if (error) throw new Error(getErrorMessage(error))
      return data
    })

    if (options.json) {
      json({
        environment: updatedEnv?.slug,
        replicas: updatedEnv?.deployment_config?.replicas ?? 1,
      })
      return
    }

    newline()
    success(`Scaled ${project}/${environment} to ${replicaCount} replica${replicaCount !== 1 ? 's' : ''}`)
    newline()
    info(`Note: Scaling takes effect on the next deployment or restart`)
  } else {
    // Display current replicas
    const currentReplicas = targetEnv.deployment_config?.replicas ?? 1

    if (options.json) {
      json({
        environment: targetEnv.slug,
        replicas: currentReplicas,
      })
      return
    }

    newline()
    header(`${icons.folder} Scale for ${project}/${environment}`)
    newline()
    keyValue('Current Replicas', String(currentReplicas))
    newline()
    info(`To scale: ${colors.muted(`temps env scale ${project} ${environment} <replicas>`)}`)
    info(`Example: ${colors.muted(`temps env scale ${project} ${environment} 3`)}`)
  }
}

// Helper function to parse .env file content
function parseEnvFile(content: string): Record<string, string> {
  const variables: Record<string, string> = {}

  for (const line of content.split('\n')) {
    const trimmed = line.trim()

    // Skip empty lines and comments
    if (!trimmed || trimmed.startsWith('#')) continue

    // Parse KEY=VALUE
    const match = trimmed.match(/^([^=]+)=(.*)$/)
    if (!match) continue

    const [, key, rawValue] = match
    let value = rawValue.trim()

    // Handle quoted values
    if ((value.startsWith('"') && value.endsWith('"')) ||
        (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1)
        .replace(/\\n/g, '\n')
        .replace(/\\"/g, '"')
        .replace(/\\'/g, "'")
    }

    variables[key.trim()] = value
  }

  return variables
}
