// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listFlags,
  createFlag,
  getFlag,
  updateFlag,
  archiveFlag,
  restoreFlag,
  setFlagEnvironment,
  getProjectBySlug,
  getEnvironments,
} from '../../api/sdk.gen.js'
import type { FlagResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  keyValue,
} from '../../ui/output.js'

type FlagValueType = 'bool' | 'string' | 'number' | 'json'

export function registerFlagsCommands(program: Command): void {
  const flags = program
    .command('flags')
    .alias('flag')
    .description('Manage feature flags (runtime config that changes without a redeploy)')

  flags
    .command('list')
    .alias('ls')
    .description('List feature flags')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <name>', 'Show values for this environment')
    .option('--include-archived', 'Include archived flags')
    .option('--page <n>', 'Page number (default: 1)')
    .option('--page-size <n>', 'Items per page (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(listFlagsCmd)

  flags
    .command('get <key>')
    .description('Show a feature flag and its per-environment values')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(getFlagCmd)

  flags
    .command('create <key>')
    .description('Create a feature flag')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-t, --type <type>', 'Value type: bool, string, number, or json')
    .requiredOption('-d, --default <value>', 'Default value, served when nothing more specific applies')
    .option('--description <text>', 'What this flag controls')
    .option(
      '--client-visible',
      'Allow this flag to be exposed to browsers (default: server-only)',
    )
    .option('--json', 'Output in JSON format')
    .action(createFlagCmd)

  flags
    .command('update <key>')
    .description('Update a flag definition (default value, description, visibility)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-d, --default <value>', 'New default value')
    .option('--description <text>', 'New description')
    .option('--client-visible', 'Expose this flag to browsers')
    .option('--no-client-visible', 'Make this flag server-only')
    .option('--json', 'Output in JSON format')
    .action(updateFlagCmd)

  flags
    .command('set <key> <value>')
    .description('Set a flag value in one environment')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .option('--json', 'Output in JSON format')
    .action(setFlagCmd)

  flags
    .command('clear <key>')
    .description('Clear a flag override so the environment inherits the default')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .action(clearFlagCmd)

  flags
    .command('disable <key>')
    .description('Kill switch: serve the default in this environment, ignoring any override')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .action((key: string, options: { project?: string; environment: string }) =>
      toggleFlagCmd(key, options, false),
    )

  flags
    .command('enable <key>')
    .description('Re-enable a flag in this environment after a kill switch')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .action((key: string, options: { project?: string; environment: string }) =>
      toggleFlagCmd(key, options, true),
    )

  flags
    .command('restore <key>')
    .description('Restore an archived flag')
    .option('-p, --project <project>', 'Project slug or ID')
    .action(restoreFlagCmd)

  flags
    .command('archive <key>')
    .description('Archive a flag (callers fall back to their own default)')
    .option('-p, --project <project>', 'Project slug or ID')
    .action(archiveFlagCmd)
}

// ============================================================================
// Helpers
// ============================================================================

async function resolveProject(projectOption?: string): Promise<{ slug: string; id: number }> {
  const resolved = await requireProjectSlug(projectOption)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { slug: resolved.slug, id: data.id }
}

async function resolveEnvironmentId(projectId: number, nameOrSlug: string): Promise<number> {
  const { data, error } = await getEnvironments({ client, path: { project_id: projectId } })
  if (error || !data) {
    throw new Error(getErrorMessage(error))
  }

  const match = data.find((e) => e.name === nameOrSlug || e.slug === nameOrSlug)
  if (!match) {
    const available = data.map((e) => e.slug).join(', ')
    throw new Error(`Environment "${nameOrSlug}" not found. Available: ${available || '(none)'}`)
  }
  return match.id
}

/**
 * Parse a value typed on the command line into the flag's declared type.
 *
 * The type comes from the flag itself rather than a CLI option, so `temps flags
 * set worker.batch_size 200` sends the number 200, not the string "200" — the
 * server rejects a type mismatch, and a shell has no types to offer.
 */
export function parseValue(raw: string, valueType: FlagValueType, key: string): unknown {
  switch (valueType) {
    case 'bool':
      if (raw === 'true') return true
      if (raw === 'false') return false
      throw new Error(`Flag "${key}" is a bool: value must be "true" or "false", got "${raw}"`)
    case 'number': {
      const n = Number(raw)
      if (!Number.isFinite(n)) {
        throw new Error(`Flag "${key}" is a number: could not parse "${raw}"`)
      }
      return n
    }
    case 'string':
      return raw
    case 'json':
      try {
        return JSON.parse(raw)
      } catch {
        throw new Error(`Flag "${key}" is json: value must be valid JSON, got "${raw}"`)
      }
    default:
      throw new Error(`Unknown value type "${valueType}" for flag "${key}"`)
  }
}

export function assertValueType(raw: string): FlagValueType {
  if (raw === 'bool' || raw === 'string' || raw === 'number' || raw === 'json') {
    return raw
  }
  throw new Error(`Invalid type "${raw}". Must be one of: bool, string, number, json`)
}

export function formatValue(value: unknown): string {
  if (value === null || value === undefined) return colors.dim('(inherits default)')
  return typeof value === 'string' ? value : JSON.stringify(value)
}

/** The value a flag resolves to in one environment, mirroring the evaluator. */
export function effectiveValue(flag: FlagResponse, environmentId: number): string {
  const override = flag.environments?.find((e) => e.environment_id === environmentId)
  if (!override) return `${formatValue(flag.default_value)} ${colors.dim('(default)')}`
  if (!override.enabled) {
    return `${formatValue(flag.default_value)} ${colors.warning('(disabled)')}`
  }
  if (override.value === null || override.value === undefined) {
    return `${formatValue(flag.default_value)} ${colors.dim('(default)')}`
  }
  return formatValue(override.value)
}

// ============================================================================
// Commands
// ============================================================================

async function listFlagsCmd(options: {
  project?: string
  environment?: string
  includeArchived?: boolean
  page?: string
  pageSize?: string
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  const { data, environmentId } = await withSpinner(
    'Fetching flags...',
    async () => {
      const environmentId = options.environment
        ? await resolveEnvironmentId(project.id, options.environment)
        : undefined

      const { data, error } = await listFlags({
        client,
        path: { project_id: project.id },
        query: {
          include_archived: options.includeArchived ?? false,
          ...(options.page ? { page: Number(options.page) } : {}),
          ...(options.pageSize ? { page_size: Number(options.pageSize) } : {}),
        },
      })
      if (error) throw new Error(getErrorMessage(error))
      return { data, environmentId }
    }
  )

  const flags = data?.flags ?? []

  if (options.json) {
    // Emit the whole envelope so scripts can page without guessing.
    json(data ?? { flags: [], total: 0, page: 1, page_size: 20, total_pages: 0 })
    return
  }

  newline()
  const scope = options.environment ? ` in ${options.environment}` : ''
  header(
    `${icons.folder} Feature flags for ${project.slug}${scope} (${data?.total ?? flags.length})`
  )

  if (flags.length === 0) {
    info('No feature flags yet. Create one with: temps flags create <key> --type bool --default false')
    newline()
    return
  }

  const columns: TableColumn<FlagResponse>[] = [
    { header: 'Key', key: 'key', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'value_type' },
    environmentId !== undefined
      ? { header: options.environment!, accessor: (f) => effectiveValue(f, environmentId) }
      : { header: 'Default', accessor: (f) => formatValue(f.default_value) },
    { header: 'Client', accessor: (f) => (f.client_visible ? 'yes' : 'no') },
    { header: 'Status', accessor: (f) => (f.archived_at ? 'archived' : 'active'), color: (value, f) => f.archived_at ? colors.dim(value) : value },
  ]

  printTable(flags, columns, { style: 'minimal' })

  const total = data?.total ?? flags.length
  const totalPages = data?.total_pages ?? 1
  if (total > flags.length) {
    info(
      `Showing ${flags.length} of ${total} (page ${data?.page ?? 1} of ${totalPages}). Use --page to see more.`
    )
  }
  newline()
}

async function getFlagCmd(
  key: string,
  options: { project?: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  const flag = await withSpinner(`Fetching flag ${key}...`, async () => {
    const { data, error } = await getFlag({ client, path: { project_id: project.id, key } })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error(`Flag "${key}" not found`)
    return data
  })

  if (options.json) {
    json(flag)
    return
  }

  newline()
  header(`${icons.folder} ${flag.key}`)
  keyValue('Type', flag.value_type)
  keyValue('Default', formatValue(flag.default_value))
  keyValue('Description', flag.description ?? '-')
  keyValue('Client visible', flag.client_visible ? 'yes' : 'no')
  // Spelled out: this is SDK-reported evaluation, not "last touched".
  keyValue(
    'Last evaluated by an app',
    flag.last_evaluated_at ?? colors.dim('never')
  )
  if (flag.archived_at) keyValue('Archived', flag.archived_at)

  newline()
  if (!flag.environments || flag.environments.length === 0) {
    info('No environment overrides — this flag serves its default everywhere.')
  } else {
    header('Environment overrides')
    printTable(
      flag.environments,
      [
        { header: 'Environment ID', accessor: (e) => String(e.environment_id) },
        { header: 'Enabled', accessor: (e) => (e.enabled ? 'yes' : 'no (kill switch)'), color: (value, e) => e.enabled ? value : colors.warning(value) },
        { header: 'Value', accessor: (e) => formatValue(e.value) },
      ],
      { style: 'minimal' },
    )
  }
  newline()
}

async function createFlagCmd(
  key: string,
  options: {
    project?: string
    type: string
    default: string
    description?: string
    clientVisible?: boolean
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)
  const valueType = assertValueType(options.type)
  const defaultValue = parseValue(options.default, valueType, key)

  const flag = await withSpinner(`Creating flag ${key}...`, async () => {
    const { data, error } = await createFlag({
      client,
      path: { project_id: project.id },
      body: {
        key,
        value_type: valueType,
        default_value: defaultValue,
        description: options.description,
        client_visible: options.clientVisible ?? false,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error('Flag creation returned no data')
    return data
  })

  if (options.json) {
    json(flag)
    return
  }

  newline()
  success(`Created flag ${colors.bold(flag.key)} (${flag.value_type}, default ${formatValue(flag.default_value)})`)
  if (!flag.client_visible) {
    info('Server-only. Pass --client-visible to expose it to browsers.')
  }
  newline()
}

async function updateFlagCmd(
  key: string,
  options: {
    project?: string
    default?: string
    description?: string
    clientVisible?: boolean
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  const flag = await withSpinner(`Updating flag ${key}...`, async () => {
    const { data: existing, error: getError } = await getFlag({
      client,
      path: { project_id: project.id, key },
    })
    if (getError) throw new Error(getErrorMessage(getError))
    if (!existing) throw new Error(`Flag "${key}" not found`)

    // Only send fields the caller actually passed: an absent field leaves the
    // stored value alone, which is not the same as clearing it.
    const body: Record<string, unknown> = {}
    if (options.default !== undefined) {
      body.default_value = parseValue(options.default, existing.value_type as FlagValueType, key)
    }
    if (options.description !== undefined) body.description = options.description
    if (options.clientVisible !== undefined) body.client_visible = options.clientVisible

    if (Object.keys(body).length === 0) {
      throw new Error('Nothing to update. Pass --default, --description, or --client-visible.')
    }

    const { data, error } = await updateFlag({
      client,
      path: { project_id: project.id, key },
      body,
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error('Flag update returned no data')
    return data
  })

  if (options.json) {
    json(flag)
    return
  }

  newline()
  success(`Updated ${colors.bold(flag.key)}`)
  newline()
}

async function setFlagCmd(
  key: string,
  value: string,
  options: { project?: string; environment: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  const result = await withSpinner(`Setting ${key} in ${options.environment}...`, async () => {
    // Read the flag first so the raw CLI string is parsed into the flag's own
    // declared type rather than guessed.
    const { data: flag, error: getError } = await getFlag({
      client,
      path: { project_id: project.id, key },
    })
    if (getError) throw new Error(getErrorMessage(getError))
    if (!flag) throw new Error(`Flag "${key}" not found`)

    const parsed = parseValue(value, flag.value_type as FlagValueType, key)
    const environmentId = await resolveEnvironmentId(project.id, options.environment)

    const { data, error } = await setFlagEnvironment({
      client,
      path: { project_id: project.id, key, environment_id: environmentId },
      body: { value: parsed },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  success(`${colors.bold(key)} = ${value} in ${colors.bold(options.environment)}`)
  info('Live within seconds — no redeploy needed.')
  newline()
}

async function clearFlagCmd(
  key: string,
  options: { project?: string; environment: string },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  await withSpinner(`Clearing ${key} in ${options.environment}...`, async () => {
    const environmentId = await resolveEnvironmentId(project.id, options.environment)
    const { error } = await setFlagEnvironment({
      client,
      path: { project_id: project.id, key, environment_id: environmentId },
      // Explicit null clears the override; an absent field would leave it alone.
      body: { value: null },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  newline()
  success(`${colors.bold(key)} now inherits its default in ${colors.bold(options.environment)}`)
  newline()
}

async function toggleFlagCmd(
  key: string,
  options: { project?: string; environment: string },
  enabled: boolean,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  await withSpinner(
    `${enabled ? 'Enabling' : 'Disabling'} ${key} in ${options.environment}...`,
    async () => {
      const environmentId = await resolveEnvironmentId(project.id, options.environment)
      const { error } = await setFlagEnvironment({
        client,
        path: { project_id: project.id, key, environment_id: environmentId },
        body: { enabled },
      })
      if (error) throw new Error(getErrorMessage(error))
    },
  )

  newline()
  if (enabled) {
    success(`${colors.bold(key)} re-enabled in ${colors.bold(options.environment)}`)
  } else {
    success(`${colors.bold(key)} disabled in ${colors.bold(options.environment)}`)
    info('It now serves its default value, ignoring any override.')
  }
  newline()
}

async function archiveFlagCmd(key: string, options: { project?: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  await withSpinner(`Archiving ${key}...`, async () => {
    const { error } = await archiveFlag({ client, path: { project_id: project.id, key } })
    if (error) throw new Error(getErrorMessage(error))
  })

  newline()
  success(`Archived ${colors.bold(key)}`)
  info('Callers now fall back to the default they compiled in.')
  newline()
}

async function restoreFlagCmd(key: string, options: { project?: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)

  await withSpinner(`Restoring ${key}...`, async () => {
    const { error } = await restoreFlag({ client, path: { project_id: project.id, key } })
    if (error) throw new Error(getErrorMessage(error))
  })

  newline()
  success(`Restored ${colors.bold(key)}`)
  info('Apps will receive it again on their next refresh.')
  newline()
}
