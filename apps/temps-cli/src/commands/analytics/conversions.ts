// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps analytics conversions ...`: conversion goals (ADR 071 D2).
 *
 * A conversion is an event (`--event signup`, optionally `--property plan=pro`)
 * or a page view (`--page /thank-you`, `*` as a wildcard). It applies to
 * events already recorded, so `list` shows its history as soon as it exists.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  createAnalyticsGoal,
  deleteAnalyticsGoal,
  getAnalyticsGoalBreakdown,
  getAnalyticsGoalStats,
  getProjectBySlug,
  listAnalyticsGoals,
  updateAnalyticsGoal,
} from '../../api/sdk.gen.js'
import type {
  AnalyticsGoal,
  AnalyticsGoalBreakdownDimension,
  AnalyticsGoalInput,
  AnalyticsGoalRevenue,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, json, colors, success, info, warning } from '../../ui/output.js'
import { parsePeriod } from './period.js'

const PERIOD_HELP = 'Time period: today, <n>h, <n>d, <n>m (e.g. 7d, 30d, 3m)'

export const BREAKDOWN_DIMENSIONS: AnalyticsGoalBreakdownDimension[] = [
  'channel',
  'referrer',
  'utm_source',
  'utm_medium',
  'utm_campaign',
  'page',
  'country',
  'device_type',
  'browser',
  'operating_system',
]

export interface GoalFlags {
  name?: string
  event?: string
  page?: string
  property?: string
  revenueProperty?: string
  currency?: string
}

interface ProjectOptions {
  project?: string
  json?: boolean
}

type BuildResult = { ok: true; body: AnalyticsGoalInput } | { ok: false; error: string }

/**
 * Request body from the create/update flags. Exactly one of `--event` and
 * `--page` says what counts; the server repeats every check.
 */
export function buildGoalInput(flags: GoalFlags): BuildResult {
  const name = flags.name?.trim()
  if (!name) return { ok: false, error: '--name is required' }
  if (Boolean(flags.event) === Boolean(flags.page)) {
    return { ok: false, error: 'Pass exactly one of --event <name> and --page <path>' }
  }
  if (flags.page) {
    if (flags.property || flags.revenueProperty || flags.currency) {
      return {
        ok: false,
        error: '--property, --revenue-property and --currency only apply to --event conversions',
      }
    }
    if (!flags.page.startsWith('/')) {
      return { ok: false, error: `--page must start with /, got "${flags.page}"` }
    }
  }
  if (Boolean(flags.revenueProperty) !== Boolean(flags.currency)) {
    return { ok: false, error: '--revenue-property and --currency go together' }
  }
  const filter: AnalyticsGoalInput['event_filter'] = []
  if (flags.property) {
    const separator = flags.property.indexOf('=')
    if (separator <= 0 || separator === flags.property.length - 1) {
      return { ok: false, error: `--property takes key=value, got "${flags.property}"` }
    }
    filter.push({
      type: 'custom_data',
      value: {
        path: flags.property.slice(0, separator).trim(),
        value: flags.property.slice(separator + 1).trim(),
      },
    })
  }
  return {
    ok: true,
    body: {
      name,
      kind: flags.event ? 'event' : 'page',
      event_name: flags.event ?? null,
      page_path: flags.page ?? null,
      event_filter: filter,
      revenue_property: flags.revenueProperty ?? null,
      revenue_currency: flags.currency?.toUpperCase() ?? null,
    },
  }
}

export function parseGoalId(raw: string | undefined): number {
  const id = Number(raw)
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`--goal-id must be a positive whole number, got "${raw ?? ''}"`)
  }
  return id
}

export function parseDimension(raw: string): AnalyticsGoalBreakdownDimension {
  const dimension = raw.replace(/-/g, '_') as AnalyticsGoalBreakdownDimension
  if (!BREAKDOWN_DIMENSIONS.includes(dimension)) {
    throw new Error(`--by must be one of ${BREAKDOWN_DIMENSIONS.join(', ')}; got "${raw}"`)
  }
  return dimension
}

export function describeGoal(goal: AnalyticsGoal): string {
  if (goal.kind === 'page') return `page ${goal.page_path ?? ''}`
  const property = goal.event_filter.find((f) => f.type === 'custom_data')
  const base = `event ${goal.event_name ?? ''}`
  return property && property.type === 'custom_data'
    ? `${base} where ${property.value.path}=${property.value.value}`
    : base
}

export function formatRevenue(revenue: AnalyticsGoalRevenue | null | undefined): string {
  if (!revenue) return '-'
  return `${revenue.total.toFixed(2)} ${revenue.currency}`
}

async function resolveProjectId(flagValue?: string): Promise<number> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
  if (error || !data) throw new Error(`Project "${resolved.slug}" not found`)
  return data.id
}

async function listConversions(options: ProjectOptions & { period: string }): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const { startDate, endDate, label } = parsePeriod(options.period)

  const [goals, stats] = await withSpinner('Fetching conversions...', async () => {
    const goals = await listAnalyticsGoals({
      client,
      path: { project_id: projectId },
      query: { page_size: 100 },
    })
    if (goals.error || !goals.data) throw new Error(getErrorMessage(goals.error))
    const stats = await getAnalyticsGoalStats({
      client,
      path: { project_id: projectId },
      query: { start_date: startDate, end_date: endDate },
    })
    if (stats.error || !stats.data) throw new Error(getErrorMessage(stats.error))
    return [goals.data, stats.data] as const
  })

  if (options.json) {
    json({ period: options.period, definitions: goals.items, ...stats })
    return
  }

  const definitions = new Map(goals.items.map((goal) => [goal.id, goal]))
  header(`Conversions (${label}), ${stats.visitors.toLocaleString('en-US')} visitors`)
  if (stats.goals.length === 0) {
    info('No conversions defined yet. Define one with:')
    info('  temps analytics conversions create --name Signup --event signup')
    info('  temps analytics conversions create --name "Thank you" --page /thank-you')
    return
  }
  printTable(stats.goals, [
    { header: 'ID', accessor: (row) => row.goal_id },
    { header: 'Name', accessor: (row) => row.name },
    {
      header: 'Matches',
      accessor: (row) => {
        const goal = definitions.get(row.goal_id)
        return goal ? describeGoal(goal) : row.kind
      },
    },
    { header: 'Visitors', accessor: (row) => row.unique_conversions, align: 'right' },
    { header: 'Total', accessor: (row) => row.total_conversions, align: 'right' },
    { header: 'Rate', accessor: (row) => `${row.conversion_rate.toFixed(1)}%`, align: 'right' },
    { header: 'Revenue', accessor: (row) => formatRevenue(row.revenue), align: 'right' },
  ])
  newline()
}

async function saveConversion(
  options: ProjectOptions & GoalFlags & { goalId?: string },
  mode: 'create' | 'update',
): Promise<void> {
  const built = buildGoalInput(options)
  if (!built.ok) throw new Error(built.error)
  const goalId = mode === 'update' ? parseGoalId(options.goalId) : undefined
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)

  const goal = await withSpinner('Saving conversion...', async () => {
    const result =
      goalId === undefined
        ? await createAnalyticsGoal({ client, path: { project_id: projectId }, body: built.body })
        : await updateAnalyticsGoal({
            client,
            path: { project_id: projectId, goal_id: goalId },
            body: built.body,
          })
    if (result.error || !result.data) throw new Error(getErrorMessage(result.error))
    return result.data
  })

  if (options.json) {
    json(goal)
    return
  }
  success(`Conversion "${goal.name}" (${describeGoal(goal)}) saved with ID ${goal.id}`)
  info('It counts events already recorded; see it with: temps analytics conversions list')
}

async function deleteConversion(
  options: ProjectOptions & { goalId?: string; force?: boolean; yes?: boolean },
): Promise<void> {
  const goalId = parseGoalId(options.goalId)
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  if (!options.force && !options.yes) {
    warning('The conversion stops appearing in reports. Recorded events are kept.')
    const confirmed = await promptConfirm({
      message: `Delete conversion ${goalId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await withSpinner('Deleting conversion...', async () => {
    const { error } = await deleteAnalyticsGoal({
      client,
      path: { project_id: projectId, goal_id: goalId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Conversion ${goalId} deleted`)
}

async function breakdownConversion(
  options: ProjectOptions & { goalId?: string; by: string; period: string; limit?: string },
): Promise<void> {
  const goalId = parseGoalId(options.goalId)
  const dimension = parseDimension(options.by)
  const limit = options.limit === undefined ? 10 : Number(options.limit)
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) {
    throw new Error(`--limit must be a whole number from 1 to 100, got "${options.limit}"`)
  }
  await requireAuth()
  await setupClient()
  const projectId = await resolveProjectId(options.project)
  const { startDate, endDate, label } = parsePeriod(options.period)

  const data = await withSpinner('Fetching breakdown...', async () => {
    const { data, error } = await getAnalyticsGoalBreakdown({
      client,
      path: { project_id: projectId, goal_id: goalId },
      query: { dimension, start_date: startDate, end_date: endDate, limit },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json({ period: options.period, ...data })
    return
  }
  header(`Conversion ${goalId} by ${dimension} (${label})`)
  printTable(data.items, [
    { header: dimension, accessor: (row) => row.value ?? '(not set)' },
    { header: 'Visitors', accessor: (row) => row.visitors, align: 'right' },
    { header: 'Converted', accessor: (row) => row.unique_conversions, align: 'right' },
    { header: 'Rate', accessor: (row) => `${row.conversion_rate.toFixed(1)}%`, align: 'right' },
    { header: 'Revenue', accessor: (row) => formatRevenue(row.revenue), align: 'right' },
  ])
  newline()
}

function goalDefinitionOptions(command: Command): Command {
  return command
    .option('-n, --name <name>', 'Name shown in reports, e.g. Signup')
    .option('--event <name>', 'Count this event, e.g. signup')
    .option('--page <path>', 'Count visits to this path; * matches anything, e.g. /docs/*')
    .option('--property <key=value>', 'Only events whose property has this value, e.g. plan=pro')
    .option('--revenue-property <key>', 'Event property holding the amount, e.g. amount')
    .option('--currency <code>', 'ISO 4217 currency of the amount, e.g. EUR')
    .option('--json', 'Output in JSON format')
}

export function registerAnalyticsConversionsCommands(analytics: Command): void {
  const conversions = analytics
    .command('conversions')
    .alias('goals')
    .description('Conversion goals: events or pages that count as success')

  conversions
    .command('list')
    .description('List conversions with visitors, rate and revenue in a period')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--period <period>', PERIOD_HELP, '30d')
    .option('--json', 'Output in JSON format')
    .action(listConversions)

  goalDefinitionOptions(
    conversions
      .command('create')
      .description('Define a conversion')
      .option('-p, --project <project>', 'Project slug or ID'),
  ).action((options) => saveConversion(options, 'create'))

  goalDefinitionOptions(
    conversions
      .command('update')
      .description("Replace a conversion's definition")
      .option('-p, --project <project>', 'Project slug or ID')
      .requiredOption('--goal-id <id>', 'Conversion ID (see list)'),
  ).action((options) => saveConversion(options, 'update'))

  conversions
    .command('delete')
    .description('Delete a conversion (recorded events are kept)')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--goal-id <id>', 'Conversion ID (see list)')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(deleteConversion)

  conversions
    .command('breakdown')
    .description('Where the visitors who converted came from')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--goal-id <id>', 'Conversion ID (see list)')
    .option('--by <dimension>', `One of ${BREAKDOWN_DIMENSIONS.join(', ')}`, 'channel')
    .option('--period <period>', PERIOD_HELP, '30d')
    .option('--limit <n>', 'Rows (default: 10, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(breakdownConversion)
}
