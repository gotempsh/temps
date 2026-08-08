import type { Command } from 'commander'
import * as fs from 'node:fs'
import * as path from 'node:path'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getEnvironments,
  getEnvironmentVariables,
  createEnvironmentVariable,
  updateEnvironmentVariable,
  getEnvironmentVariableValue,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptSelect, promptConfirm, promptCheckbox } from '../../ui/prompts.js'
import {
  success,
  info,
  warning,
  newline,
  colors,
  error as errorOutput,
} from '../../ui/output.js'

export function findEnvironment<T extends { name: string; slug: string }>(
  envs: T[],
  query: string
): T | undefined {
  return envs.find(e => e.name.toLowerCase() === query.toLowerCase() || e.slug === query)
}

export function formatEnvLine(key: string, value: string): string {
  const escapedValue =
    value.includes('\n') || value.includes('"')
      ? `"${value.replace(/"/g, '\\"').replace(/\n/g, '\\n')}"`
      : value.includes(' ') || value.includes('#')
        ? `"${value}"`
        : value
  return `${key}=${escapedValue}`
}

async function resolveProjectId(slug: string): Promise<number> {
  const { data, error } = await getProjectBySlug({
    client,
    path: { slug },
  })
  if (error || !data) {
    throw new Error(`Project "${slug}" not found`)
  }
  return data.id
}

async function pull(
  file: string | undefined,
  options: { environment?: string; project?: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  const outputFile = file ?? '.env'

  const result = await withSpinner('Fetching environment variables...', async () => {
    const projectId = await resolveProjectId(resolved.slug)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({ client, path: { project_id: projectId } }),
      getEnvironments({ client, path: { project_id: projectId } }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return {
      projectId,
      vars: varsResult.data ?? [],
      envs: envsResult.data ?? [],
    }
  })

  let filteredVars = result.vars

  // Filter by environment
  if (options.environment) {
    const targetEnv = findEnvironment(result.envs, options.environment)
    if (!targetEnv) {
      errorOutput(`Environment "${options.environment}" not found`)
      info(`Available: ${result.envs.map(e => e.name).join(', ')}`)
      return
    }
    filteredVars = result.vars.filter(v =>
      v.environments.some(e => e.id === targetEnv.id)
    )
  } else if (result.envs.length > 1) {
    // Prompt for environment
    const envName = await promptSelect({
      message: 'Pull from which environment?',
      choices: result.envs.map(e => ({
        name: e.name,
        value: e.name,
        description: e.is_preview ? 'Preview' : undefined,
      })),
    })
    const targetEnv = result.envs.find(e => e.name === envName)
    if (targetEnv) {
      filteredVars = result.vars.filter(v =>
        v.environments.some(e => e.id === targetEnv.id)
      )
    }
  }

  if (filteredVars.length === 0) {
    warning('No environment variables to pull')
    return
  }

  // Write-only secrets have no readable value at all. Drop them, but say so
  // explicitly rather than emitting an empty assignment that would blank the
  // variable wherever this file gets loaded.
  const secretVars = filteredVars.filter(v => v.is_secret)
  const exportableVars = filteredVars.filter(v => !v.is_secret)

  if (secretVars.length > 0) {
    warning(
      `Skipping ${secretVars.length} write-only secret(s): ${secretVars.map(v => v.key).join(', ')}. ` +
        'Their values are never returned by the API — set them by hand where you need them.'
    )
  }

  // The list endpoint masks every value as `***`; plaintext only comes from the
  // audited per-key reveal endpoint. Without this the pull would overwrite the
  // developer's .env with a file full of `***`.
  // Keyed by row id: the same name can exist in several environments.
  const resolvedValues = new Map<number, string>()
  const unreadable: string[] = []
  await withSpinner('Resolving values...', async () => {
    for (const v of exportableVars) {
      const valueResult = await getEnvironmentVariableValue({
        client,
        path: { project_id: result.projectId, key: v.key },
        query: { var_id: v.id },
      })
      if (valueResult.error || !valueResult.data) {
        unreadable.push(v.key)
        continue
      }
      resolvedValues.set(v.id, valueResult.data.value)
    }
  })

  if (unreadable.length > 0) {
    errorOutput(
      `Could not read ${unreadable.length} value(s): ${unreadable.join(', ')}. ` +
        'Reading plaintext needs the secrets:read permission. Nothing was written — ' +
        `overwriting ${outputFile} with partial values would blank these variables.`
    )
    process.exitCode = 1
    return
  }

  // Generate .env content
  const envContent = exportableVars
    .map(v => formatEnvLine(v.key, resolvedValues.get(v.id) ?? ''))
    .join('\n')

  const outputPath = path.isAbsolute(outputFile)
    ? outputFile
    : path.resolve(process.cwd(), outputFile)

  // Check if file exists and confirm overwrite
  if (fs.existsSync(outputPath)) {
    const overwrite = await promptConfirm({
      message: `${outputFile} already exists. Overwrite?`,
      default: false,
    })
    if (!overwrite) {
      info('Cancelled')
      return
    }
  }

  fs.writeFileSync(outputPath, envContent + '\n')
  success(`Pulled ${exportableVars.length} variables to ${outputFile}`)
}

async function push(
  file: string | undefined,
  options: { environment?: string; project?: string; overwrite?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  const inputFile = file ?? '.env'

  const inputPath = path.isAbsolute(inputFile)
    ? inputFile
    : path.resolve(process.cwd(), inputFile)

  if (!fs.existsSync(inputPath)) {
    errorOutput(`File not found: ${inputFile}`)
    return
  }

  // Parse .env file
  const content = fs.readFileSync(inputPath, 'utf-8')
  const variables = parseEnvFile(content)

  if (Object.keys(variables).length === 0) {
    warning('No variables found in file')
    return
  }

  info(`Found ${Object.keys(variables).length} variables in ${inputFile}`)

  const result = await withSpinner('Fetching environments...', async () => {
    const projectId = await resolveProjectId(resolved.slug)

    const [varsResult, envsResult] = await Promise.all([
      getEnvironmentVariables({ client, path: { project_id: projectId } }),
      getEnvironments({ client, path: { project_id: projectId } }),
    ])

    if (varsResult.error) throw new Error(getErrorMessage(varsResult.error))
    if (envsResult.error) throw new Error(getErrorMessage(envsResult.error))

    return {
      projectId,
      vars: varsResult.data ?? [],
      envs: envsResult.data ?? [],
    }
  })

  if (result.envs.length === 0) {
    errorOutput('No environments found. Create an environment first.')
    return
  }

  // Determine which environments to push to
  let environmentIds: number[]
  if (options.environment) {
    const envNames = options.environment.split(',').map(n => n.trim().toLowerCase())
    environmentIds = []
    for (const name of envNames) {
      const env = findEnvironment(result.envs, name)
      if (!env) {
        errorOutput(`Environment "${name}" not found`)
        info(`Available: ${result.envs.map(e => e.name).join(', ')}`)
        return
      }
      environmentIds.push(env.id)
    }
  } else {
    const selected = await promptCheckbox({
      message: 'Push to which environments?',
      choices: result.envs.map(e => ({
        name: `${e.name} ${e.is_preview ? '(preview)' : ''}`,
        value: e.id,
      })),
    })
    environmentIds = selected
  }

  let created = 0
  let updated = 0
  let skipped = 0

  for (const [key, value] of Object.entries(variables)) {
    const existing = result.vars.find(v => v.key === key)

    if (existing) {
      if (options.overwrite) {
        try {
          await updateEnvironmentVariable({
            client,
            path: { project_id: result.projectId, var_id: existing.id },
            body: { key, value, environment_ids: environmentIds, include_in_preview: true },
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
          path: { project_id: result.projectId },
          body: { key, value, environment_ids: environmentIds, include_in_preview: true },
        })
        created++
      } catch (e) {
        warning(`Failed to create ${key}: ${getErrorMessage(e)}`)
      }
    }
  }

  newline()
  success(`Push complete: ${created} created, ${updated} updated, ${skipped} skipped`)
  if (skipped > 0 && !options.overwrite) {
    info(`Use ${colors.bold('--overwrite')} to update existing variables`)
  }
}

export function parseEnvFile(content: string): Record<string, string> {
  const variables: Record<string, string> = {}

  for (const line of content.split('\n')) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.startsWith('#')) continue

    const match = trimmed.match(/^([^=]+)=(.*)$/)
    if (!match) continue

    const [, key, rawValue] = match
    if (!key || rawValue === undefined) continue

    let value = rawValue.trim()
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

export function registerEnvSyncCommands(program: Command): void {
  program
    .command('env:pull [file]')
    .description('Pull environment variables to a .env file')
    .option('-e, --environment <name>', 'Pull from specific environment')
    .option('-p, --project <project>', 'Project slug')
    .action(pull)

  program
    .command('env:push [file]')
    .description('Push environment variables from a .env file')
    .option('-e, --environment <names>', 'Comma-separated environment names')
    .option('-p, --project <project>', 'Project slug')
    .option('--overwrite', 'Overwrite existing variables')
    .action(push)
}
