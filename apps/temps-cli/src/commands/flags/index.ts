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
  setFlagRules,
  reshuffleFlag,
  evaluateFlag,
  getProjectBySlug,
  getEnvironments,
} from '../../api/sdk.gen.js'
import type { FlagResponse, FlagRule, FlagVariant } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
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
    .option(
      '--variants <json|@file>',
      'Replace the variants, as a JSON array of {key, value, description?} (or @path to a file)',
    )
    .option(
      '--exposure-rate <percent>',
      'Share of visitors (0-100) whose browser records a flag_exposure event; 0 turns it off',
    )
    .option('--json', 'Output in JSON format')
    .action(updateFlagCmd)

  const rules = flags
    .command('rules')
    .description('Targeting rules: serve a different value to some users, or to a percentage of them')

  rules
    .command('list <key>')
    .alias('ls')
    .description('Show the ordered rules of a flag in one environment')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .option('--json', 'Output the rules as JSON (the format `rules set` reads)')
    .action(listRulesCmd)

  rules
    .command('set <key> <rules>')
    .description(
      'Replace the rules of a flag in one environment, from a JSON array (or @path to a file, or - for stdin)',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .option('--json', 'Output in JSON format')
    .action(setRulesCmd)

  rules
    .command('clear <key>')
    .description('Remove every rule of a flag in one environment')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .action(clearRulesCmd)

  flags
    .command('evaluate <key>')
    .alias('eval')
    .description('Show what a user would get, and why, in one environment')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('-e, --environment <name>', 'Environment name or slug')
    .option('-s, --subject <key>', 'Bucketing subject, e.g. a user id')
    .option(
      '-a, --attr <name=value>',
      'Targeting attribute (repeatable)',
      (value: string, previous: string[] = []) => [...previous, value],
      [] as string[],
    )
    .option('--json', 'Output in JSON format')
    .action(evaluateFlagCmd)

  flags
    .command('reshuffle <key>')
    .description(
      'Draw new rollout buckets for every rollout and split of this flag (never during an experiment)',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-y, --yes', 'Skip the confirmation')
    .action(reshuffleFlagCmd)

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

/**
 * Read a JSON argument: inline JSON, `@path` for a file, or `-` for stdin.
 */
export async function readJsonArgument(raw: string, what: string): Promise<unknown> {
  let text = raw
  if (raw === '-') {
    text = await Bun.stdin.text()
  } else if (raw.startsWith('@')) {
    const path = raw.slice(1)
    const file = Bun.file(path)
    if (!(await file.exists())) {
      throw new Error(`Cannot read ${what}: file "${path}" does not exist`)
    }
    text = await file.text()
  }
  try {
    return JSON.parse(text)
  } catch (error) {
    throw new Error(
      `${what} must be valid JSON: ${error instanceof Error ? error.message : String(error)}`,
    )
  }
}

/** `name=value` pairs into typed attributes (numbers and booleans kept typed). */
export function parseAttributes(pairs: string[]): Record<string, string | number | boolean> {
  const attributes: Record<string, string | number | boolean> = {}
  for (const pair of pairs) {
    const separator = pair.indexOf('=')
    if (separator <= 0) {
      throw new Error(`Attribute "${pair}" must be written as name=value`)
    }
    const name = pair.slice(0, separator).trim()
    const raw = pair.slice(separator + 1).trim()
    attributes[name] =
      raw === 'true'
        ? true
        : raw === 'false'
          ? false
          : /^-?\d+(\.\d+)?$/.test(raw)
            ? Number(raw)
            : raw
  }
  return attributes
}

/** Percent typed on the command line into the stored 0..1 rate. */
export function parseExposureRate(raw: string): number {
  const percent = Number(raw.replace(/%$/, ''))
  if (raw.trim() === '' || !Number.isFinite(percent) || percent < 0 || percent > 100) {
    throw new Error(`--exposure-rate must be a percentage between 0 and 100, got "${raw}"`)
  }
  return percent / 100
}

const OPERATOR_WORDS: Record<string, string> = {
  eq: '=',
  neq: '!=',
  in: 'in',
  not_in: 'not in',
  contains: 'contains',
  not_contains: 'does not contain',
  matches: 'matches',
  gt: '>',
  gte: '>=',
  lt: '<',
  lte: '<=',
  exists: 'is set',
  not_exists: 'is not set',
}

/** One line per rule, e.g. `plan in [pro, team] (25%) -> true`. */
export function describeRule(rule: FlagRule): string {
  const conditions =
    rule.conditions && rule.conditions.length > 0
      ? rule.conditions
          .map((condition) => {
            const word = OPERATOR_WORDS[condition.operator] ?? condition.operator
            if (condition.value === undefined || condition.value === null) {
              return `${condition.attribute} ${word}`
            }
            const operand = Array.isArray(condition.value)
              ? `[${condition.value.map(String).join(', ')}]`
              : String(condition.value)
            return `${condition.attribute} ${word} ${operand}`
          })
          .join(' and ')
      : 'everyone'
  const rollout =
    rule.rollout_percentage !== undefined &&
    rule.rollout_percentage !== null &&
    rule.rollout_percentage < 100
      ? ` (${rule.rollout_percentage}%)`
      : ''
  const serve = rule.serve
  let served: string
  switch (serve.kind) {
    case 'value':
      served = formatValue(serve.value)
      break
    case 'variant':
      served = `variant ${serve.variant}`
      break
    case 'split':
      served = serve.split.map((arm) => `${arm.variant} ${arm.weight}%`).join(' / ')
      break
    default:
      served = JSON.stringify(serve)
  }
  return `${conditions}${rollout} -> ${served}`
}

/** What a reason means, numbering rules from one as the console does. */
export function describeReason(reason: { kind: string; index?: number }): string {
  switch (reason.kind) {
    case 'RULE_MATCH':
      return `rule ${(reason.index ?? 0) + 1} matched`
    case 'PERCENTAGE_ROLLOUT':
      return `rule ${(reason.index ?? 0) + 1} matched and the subject is in its rollout`
    case 'ENVIRONMENT_VALUE':
      return "no rule matched; the environment's value"
    case 'DEFAULT':
      return 'no rule matched; the default'
    case 'DISABLED':
      return 'disabled in this environment; the default'
    case 'FLAG_NOT_FOUND':
      return 'the flag does not exist or is archived'
    case 'ERROR':
      return 'the stored rules could not be evaluated; the default'
    default:
      return reason.kind
  }
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
  keyValue(
    'Variants',
    flag.variants && flag.variants.length > 0
      ? flag.variants.map((v) => `${v.key}=${formatValue(v.value)}`).join(', ')
      : '-',
  )
  keyValue(
    'Exposure events',
    flag.exposure_sample_rate > 0 ? `${flag.exposure_sample_rate * 100}% of visitors` : 'off',
  )
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
        {
          header: 'Rules',
          accessor: (e) =>
            e.rules_valid === false ? 'unreadable' : String(e.rules?.length ?? 0),
        },
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
    variants?: string
    exposureRate?: string
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProject(options.project)
  const variants =
    options.variants !== undefined
      ? await readJsonArgument(options.variants, '--variants')
      : undefined
  if (variants !== undefined && !Array.isArray(variants)) {
    throw new Error('--variants must be a JSON array of {key, value, description?}')
  }
  const exposureRate =
    options.exposureRate !== undefined ? parseExposureRate(options.exposureRate) : undefined

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
    if (variants !== undefined) body.variants = variants as FlagVariant[]
    if (exposureRate !== undefined) body.exposure_sample_rate = exposureRate

    if (Object.keys(body).length === 0) {
      throw new Error(
        'Nothing to update. Pass --default, --description, --client-visible, --variants or --exposure-rate.',
      )
    }
    // Refuse the write if someone changed the flag since it was read above.
    body.expected_updated_at = existing.updated_at

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

// ============================================================================
// Rules, evaluation, reshuffle
// ============================================================================

async function loadFlagAndEnvironment(
  projectOption: string | undefined,
  key: string,
  environment: string,
): Promise<{ projectId: number; flag: FlagResponse; environmentId: number }> {
  const project = await resolveProject(projectOption)
  const { data: flag, error } = await getFlag({ client, path: { project_id: project.id, key } })
  if (error) throw new Error(getErrorMessage(error))
  if (!flag) throw new Error(`Flag "${key}" not found`)
  const environmentId = await resolveEnvironmentId(project.id, environment)
  return { projectId: project.id, flag, environmentId }
}

async function listRulesCmd(
  key: string,
  options: { project?: string; environment: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const { flag, environmentId } = await withSpinner(`Fetching rules for ${key}...`, () =>
    loadFlagAndEnvironment(options.project, key, options.environment),
  )
  const row = flag.environments?.find((e) => e.environment_id === environmentId)
  const rules = row?.rules ?? []

  if (options.json) {
    json(rules)
    return
  }

  newline()
  header(`${icons.folder} Rules for ${flag.key} in ${options.environment}`)
  if (row && row.rules_valid === false) {
    info(colors.warning('The stored rules could not be read; this environment serves the default (reason ERROR). Replace them with `temps flags rules set`.'))
  }
  if (rules.length === 0) {
    info('No rules. Everyone gets the environment value (or the default).')
  } else {
    rules.forEach((rule, index) => {
      const label = rule.description ? `${rule.description}: ` : ''
      console.log(`  ${colors.bold(String(index + 1))}. ${label}${describeRule(rule)}`)
    })
    info('First match wins. Users matching no rule get the environment value.')
  }
  newline()
}

async function setRulesCmd(
  key: string,
  rulesArgument: string,
  options: { project?: string; environment: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const rules = await readJsonArgument(rulesArgument, 'rules')
  if (!Array.isArray(rules)) {
    throw new Error('rules must be a JSON array; see `temps flags rules list <key> -e <env> --json`')
  }

  const result = await withSpinner(`Saving rules for ${key} in ${options.environment}...`, async () => {
    const { projectId, flag, environmentId } = await loadFlagAndEnvironment(
      options.project,
      key,
      options.environment,
    )
    const row = flag.environments?.find((e) => e.environment_id === environmentId)
    const { data, error } = await setFlagRules({
      client,
      path: { project_id: projectId, key, environment_id: environmentId },
      // The revision read above: a concurrent edit is refused with 409.
      body: { rules: rules as FlagRule[], expected_updated_at: row?.updated_at ?? null },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  success(`${rules.length} rule(s) saved for ${colors.bold(key)} in ${colors.bold(options.environment)}`)
  newline()
}

async function clearRulesCmd(
  key: string,
  options: { project?: string; environment: string },
): Promise<void> {
  await requireAuth()
  await setupClient()

  await withSpinner(`Clearing rules for ${key} in ${options.environment}...`, async () => {
    const { projectId, flag, environmentId } = await loadFlagAndEnvironment(
      options.project,
      key,
      options.environment,
    )
    const row = flag.environments?.find((e) => e.environment_id === environmentId)
    const { error } = await setFlagRules({
      client,
      path: { project_id: projectId, key, environment_id: environmentId },
      body: { rules: [], expected_updated_at: row?.updated_at ?? null },
    })
    if (error) throw new Error(getErrorMessage(error))
  })

  newline()
  success(`Rules cleared for ${colors.bold(key)} in ${colors.bold(options.environment)}`)
  newline()
}

async function evaluateFlagCmd(
  key: string,
  options: { project?: string; environment: string; subject?: string; attr: string[]; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  const attributes = parseAttributes(options.attr ?? [])
  const project = await resolveProject(options.project)

  const evaluation = await withSpinner(`Evaluating ${key}...`, async () => {
    const environmentId = await resolveEnvironmentId(project.id, options.environment)
    const { data, error } = await evaluateFlag({
      client,
      path: { project_id: project.id, key },
      body: { environment_id: environmentId, key: options.subject ?? null, attributes },
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error('Evaluation returned no data')
    return data
  })

  if (options.json) {
    json(evaluation)
    return
  }

  newline()
  keyValue('Value', formatValue(evaluation.value))
  keyValue('Variant', evaluation.variant ?? '-')
  keyValue('Why', describeReason(evaluation.reason))
  newline()
}

async function reshuffleFlagCmd(key: string, options: { project?: string; yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Reshuffle ${key}? Users in partial rollouts and splits may switch value, and a running experiment on it becomes meaningless.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const project = await resolveProject(options.project)
  await withSpinner(`Reshuffling ${key}...`, async () => {
    const { error } = await reshuffleFlag({ client, path: { project_id: project.id, key } })
    if (error) throw new Error(getErrorMessage(error))
  })

  newline()
  success(`New rollout buckets drawn for ${colors.bold(key)}`)
  newline()
}
