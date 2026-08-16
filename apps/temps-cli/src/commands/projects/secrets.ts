import type { Command } from 'commander'
import { readFileSync } from 'node:fs'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getProjectBySlug,
  getEnvironments,
  listProjectSecrets,
  createProjectSecret,
  updateProjectSecret,
  deleteProjectSecret,
} from '../../api/sdk.gen.js'
import type { ProjectSecretResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning } from '../../ui/output.js'

// Project secrets are mounted into the deployed container as FILES at
// /run/secrets/<KEY> (mode 0400) on the next deployment — distinct from the
// top-level `temps secrets` command, which manages agent/MCP-sandbox-scoped
// secrets (a completely different store, referenced via ${TEMPS_SECRET:name}
// in MCP config, unrelated to any specific project's own deployment).

interface ListOptions {
  project?: string
  environment?: string
  json?: boolean
}

interface CreateOptions {
  project?: string
  key: string
  value: string
  environment: string[]
  includeInPreview?: boolean
}

interface UpdateOptions {
  project?: string
  key: string
  value?: string
  environment: string[]
  includeInPreview?: boolean
}

interface DeleteOptions {
  project?: string
  force?: boolean
  yes?: boolean
}

function collect(value: string, previous: string[]): string[] {
  return [...previous, value]
}

/** Resolve value: if prefixed with @, read from a local file path. */
export function resolveValue(value: string): string {
  if (value.startsWith('@')) {
    const filePath = value.slice(1)
    try {
      return readFileSync(filePath, 'utf-8')
    } catch (e) {
      throw new Error(`Failed to read file '${filePath}': ${e}`)
    }
  }
  return value
}

async function getProjectId(projectSlug: string): Promise<number> {
  const { data, error } = await getProjectBySlug({ client, path: { slug: projectSlug } })
  if (error || !data) {
    throw new Error(`Project "${projectSlug}" not found`)
  }
  return data.id
}

/** Environment names -> ids — the create/update API takes environment_ids. */
async function resolveEnvironmentIds(projectId: number, names: string[]): Promise<number[]> {
  const { data, error } = await getEnvironments({ client, path: { project_id: projectId } })
  if (error) throw new Error(getErrorMessage(error))
  const byName = new Map((data ?? []).map((e) => [e.name, e.id]))
  return names.map((name) => {
    const id = byName.get(name)
    if (id === undefined) throw new Error(`Environment "${name}" not found on this project`)
    return id
  })
}

/** The update/delete APIs address secrets by numeric id, not key — list first. */
async function findSecretByKey(projectId: number, key: string): Promise<ProjectSecretResponse> {
  const { data, error } = await listProjectSecrets({ client, path: { project_id: projectId } })
  if (error) throw new Error(getErrorMessage(error))
  const found = (data ?? []).find((s) => s.key === key)
  if (!found) throw new Error(`Secret "${key}" not found in this project`)
  return found
}

export function registerProjectSecretsCommands(projects: Command): void {
  const secrets = projects
    .command('secrets')
    .description(
      'Manage project secrets — mounted into the deployed container as files at /run/secrets/<KEY> (mode 0400), not environment variables. Distinct from `temps secrets` (agent/MCP-sandbox-scoped).',
    )

  secrets
    .command('list')
    .alias('ls')
    .description('List secrets for a project (values are never returned)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <name>', 'Filter to one environment')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  secrets
    .command('create')
    .alias('add')
    .description('Create a project secret (mounted at /run/secrets/<KEY> on the next deployment)')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption(
      '-k, --key <key>',
      'Secret key — becomes the filename at /run/secrets/<KEY>. Letters, digits, underscore; must start with a letter or underscore.',
    )
    .requiredOption(
      '-v, --value <value>',
      'Secret value (<=1 MiB). Prefix with @ to read from a local file, e.g. @./auth.json — never touches shell history.',
    )
    .option('-e, --environment <name>', 'Scope to one environment (repeatable; default: all)', collect, [])
    .option('--include-in-preview', 'Also mount this secret in preview environments')
    .action(createAction)

  secrets
    .command('update')
    .description('Update a project secret (a redeploy is required for running containers to pick it up)')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-k, --key <key>', 'Key of the secret to update')
    .option(
      '-v, --value <value>',
      'New value (<=1 MiB). Prefix with @ to read from a local file. Omit to keep the existing value.',
    )
    .option('-e, --environment <name>', 'Replace environment scoping (repeatable)', collect, [])
    .option('--include-in-preview', 'Include in preview environments')
    .option('--no-include-in-preview', 'Exclude from preview environments')
    .action(updateAction)

  secrets
    .command('delete')
    .alias('rm')
    .description('Delete a project secret')
    .argument('<key>', 'Key of the secret to delete')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(deleteAction)
}

// --- Actions ---

async function listAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const items = await withSpinner('Fetching secrets...', async () => {
    const projectId = await getProjectId(resolved.slug)
    let environmentId: number | undefined
    if (options.environment) {
      ;[environmentId] = await resolveEnvironmentIds(projectId, [options.environment])
    }
    const { data, error } = await listProjectSecrets({
      client,
      path: { project_id: projectId },
      query: environmentId !== undefined ? { environment_id: environmentId } : undefined,
    })
    if (error) throw new Error(getErrorMessage(error))
    return data ?? []
  })

  if (options.json) {
    json(items)
    return
  }

  newline()
  header(`${icons.info} Project secrets for ${resolved.slug} (${items.length})`)

  if (items.length === 0) {
    info('No secrets defined yet.')
    info(`Example: temps projects secrets create -p ${resolved.slug} -k CODEX_AUTH -v @./auth.json`)
    newline()
    return
  }

  for (const secret of items) {
    console.log(`  ${colors.primary(secret.key)}`)
    console.log(`    ${colors.muted('Mount path:')} ${colors.bold(`/run/secrets/${secret.key}`)}`)
    const scope =
      secret.environments.length > 0 ? secret.environments.map((e) => e.name).join(', ') : 'all environments'
    console.log(
      `    ${colors.muted('Environments:')} ${scope}${secret.include_in_preview ? ' (+ preview)' : ''}`,
    )
    newline()
  }
}

async function createAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const value = resolveValue(options.value)

  const secret = await withSpinner('Saving secret...', async () => {
    const projectId = await getProjectId(resolved.slug)
    const environmentIds =
      options.environment.length > 0 ? await resolveEnvironmentIds(projectId, options.environment) : undefined

    const { data, error } = await createProjectSecret({
      client,
      path: { project_id: projectId },
      body: {
        key: options.key,
        value,
        ...(environmentIds ? { environment_ids: environmentIds } : {}),
        ...(options.includeInPreview !== undefined ? { include_in_preview: options.includeInPreview } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data!
  })

  success(`Secret created: ${secret.key}`)
  info(`Mounted at ${colors.bold(`/run/secrets/${secret.key}`)} (mode 0400) on the next deployment`)
  info('A redeploy is required for this to take effect.')
}

async function updateAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  if (options.value === undefined && options.environment.length === 0 && options.includeInPreview === undefined) {
    warning('No fields to update. Provide at least one of --value, --environment, or --include-in-preview')
    return
  }

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const secret = await withSpinner('Updating secret...', async () => {
    const projectId = await getProjectId(resolved.slug)
    const current = await findSecretByKey(projectId, options.key)
    const environmentIds =
      options.environment.length > 0 ? await resolveEnvironmentIds(projectId, options.environment) : undefined

    const { data, error } = await updateProjectSecret({
      client,
      path: { project_id: projectId, secret_id: current.id },
      body: {
        ...(options.value !== undefined ? { value: resolveValue(options.value) } : {}),
        ...(environmentIds ? { environment_ids: environmentIds } : {}),
        ...(options.includeInPreview !== undefined ? { include_in_preview: options.includeInPreview } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data!
  })

  success(`Secret updated: ${secret.key}`)
  info('A redeploy is required for running containers to pick up the change.')
}

async function deleteAction(key: string, options: DeleteOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const skipConfirmation = options.force || options.yes
  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete secret "${key}" from ${resolved.slug}? This cannot be undone.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting secret...', async () => {
    const projectId = await getProjectId(resolved.slug)
    const current = await findSecretByKey(projectId, key)
    const { error } = await deleteProjectSecret({
      client,
      path: { project_id: projectId, secret_id: current.id },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  success(`Secret "${key}" deleted`)
  info('Running containers keep their currently-mounted value until redeployed.')
}
