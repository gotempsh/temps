// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  info,
  warning,
  keyValue,
} from '../../ui/output.js'

// TODO: replace these local shapes with the generated `GlobalLogSearchRequest`
// / `GlobalLogSearchResponse` / `GlobalLogFacetsRequest` / `GlobalLogFacetsResponse`
// types once `apps/temps-cli/openapi.json` is refreshed against a server that
// serves the indexed log store (`bun run spec:update && bun run generate:api`).
// The committed snapshot still describes the old chunk-scan endpoints, so the
// generated `searchGlobalLogs` would type away `services`, `container_ids`,
// `stream` and `line_id`, and has no `facetGlobalLogs` at all.

type LogLevel = 'TRACE' | 'DEBUG' | 'INFO' | 'WARN' | 'ERROR'
type LogSourceKind = 'collected' | 'application' | 'service'

interface GlobalLogSearchRequest {
  start_time: string
  end_time: string
  source?: LogSourceKind
  projects?: string[]
  external_services?: string[]
  scopes?: string[]
  levels?: LogLevel[]
  envs?: string[]
  services?: string[]
  container_ids?: string[]
  node_ids?: number[]
  deploy_id?: number
  text?: string
  cursor?: string
  page_size?: number
  /** `<key><op><value>` predicates, ops: `=`, `!=`, `^=`, `>`, `<`, or `<key>?` for exists. */
  attrs?: string[]
}

/**
 * The subset of `GlobalLogSearchRequest`'s filter fields the analytics GET
 * endpoints (`attributes`, `facets/attrs`, `histogram`, `aggregate`) accept
 * as repeated query parameters: everything except `text`
 * (the index holds no message bytes), `cursor`/`page_size` (analytics reads
 * are not paginated pages of lines) and `attrs` (each endpoint below takes
 * its own attribute predicates under its own `attr` param, since some also
 * take group/facet keys over the same syntax).
 */
type AnalyticsFilterQuery = Omit<
  GlobalLogSearchRequest,
  'text' | 'cursor' | 'page_size' | 'attrs'
>

export function toAnalyticsQuery(filters: GlobalLogSearchRequest): AnalyticsFilterQuery {
  const { text: _text, cursor: _cursor, page_size: _pageSize, attrs: _attrs, ...rest } = filters
  return rest
}

/** Console route where the ClickHouse line index (analytics) is configured. */
const ANALYTICS_SETUP_PATH = '/settings/metrics-monitoring'
const MAX_ANALYTICS_LIMIT = 1000

/**
 * GET one of the attribute-analytics endpoints. On a 503 ("line index
 * unavailable" — no ClickHouse configured, or the reindexer hasn't caught
 * up) the server's `detail` already explains the reason; append where to
 * fix it rather than leaving the operator to guess.
 */
async function fetchAnalytics<T>(
  url: string,
  query: Record<string, unknown>,
): Promise<T> {
  const { data, error, response } = await client.get({ url, query })
  if (error) {
    const message = getErrorMessage(error)
    if (response?.status === 503) {
      throw new Error(`${message} Configure it at ${ANALYTICS_SETUP_PATH}.`)
    }
    throw new Error(message)
  }
  return data as T
}

/** Mirrors the server's `parse_attr_predicate` so a malformed `--attr`
 * fails at the CLI instead of round-tripping to the server first. */
export function validateAttrPredicate(raw: string): string {
  const invalid = () =>
    new Error(
      `Invalid --attr "${raw}". Expected <key><op><value> with op in =, !=, ^= (prefix), ` +
        '>, < or <key>? for "exists".',
    )
  if (raw.endsWith('?')) {
    if (raw.length === 1) throw invalid()
    return raw
  }
  // Two-character operators first: `!=`/`^=` both contain a character that
  // could also be misread as a one-character operator.
  const twoChar = ['!=', '^='].find((token) => raw.includes(token))
  if (twoChar) {
    if (raw.indexOf(twoChar) === 0) throw invalid()
    return raw
  }
  const oneChar = ['=', '>', '<'].find((ch) => raw.includes(ch))
  if (oneChar) {
    if (raw.indexOf(oneChar) === 0) throw invalid()
    return raw
  }
  throw invalid()
}

/** Mirrors the server's `parse_group_key`: a label name or `attr:<name>`. */
export function validateGroupKey(raw: string): string {
  const trimmed = raw.trim()
  if (trimmed.startsWith('attr:')) {
    if (trimmed === 'attr:')
      throw new Error(`Invalid group key "${raw}". attr: group key needs a name, e.g. attr:worker.`)
    return trimmed
  }
  if (!FACET_FIELDS.includes(trimmed as FacetField)) {
    throw new Error(
      `Invalid group key "${raw}". Use one of: ${FACET_FIELDS.join(', ')}, or attr:<name>.`,
    )
  }
  return trimmed
}

/** Mirrors the server's `parse_group_keys`: a comma-separated list. */
export function validateGroupKeys(raw: string): string {
  const keys = raw
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
  if (keys.length === 0) throw new Error('At least one group-by key is required.')
  return keys.map(validateGroupKey).join(',')
}

const METRIC_FNS = ['count_distinct', 'avg', 'p50', 'p95', 'p99', 'max', 'sum']

/** Mirrors the server's `parse_metric`. */
export function validateMetric(raw: string): string {
  if (raw === 'count') return raw
  const idx = raw.indexOf(':')
  const name = idx > 0 ? raw.slice(0, idx) : ''
  const attr = idx > 0 ? raw.slice(idx + 1) : ''
  if (!name || !attr || !METRIC_FNS.includes(name)) {
    throw new Error(
      `Invalid --metric "${raw}". Expected count, or one of ${METRIC_FNS.join(', ')} ` +
        'followed by :<attr>.',
    )
  }
  return raw
}

interface GlobalLogLine {
  timestamp: string
  level: LogLevel
  stream: 'stdout' | 'stderr'
  service: string
  message: string
  fields?: Record<string, unknown>
  /**
   * Decimal string, not a number: a 64-bit value seeded from Unix nanoseconds,
   * past `Number.MAX_SAFE_INTEGER`. Print it and compare it; never parse it.
   */
  line_id: string
  container_id?: string
  deploy_id?: number | null
  node_id?: number | null
  node_name?: string | null
  project_id?: number | null
  external_service_id?: number | null
  owner: string
  env: string
}

interface GlobalLogSearchResponse {
  lines: GlobalLogLine[]
  /**
   * Still populated on a partial page — press on with `next_cursor` to keep
   * searching. `null` on the true last page means there is genuinely nothing
   * older, never an exhausted budget presented as "done".
   */
  next_cursor?: string | null
  /** True when the store's time/byte budget ran out before this page could
   * be proven complete. `scanned_back_to` says how far it got. */
  partial?: boolean
  /** Set when `partial` is true: everything newer than this has been
   * searched; older lines have not been reached yet. */
  scanned_back_to?: string | null
}

const FACET_FIELDS = [
  'env',
  'service',
  'level',
  'stream',
  'project',
  'external_service',
  'node',
  'deploy',
  'container',
] as const
type FacetField = (typeof FACET_FIELDS)[number]

interface FacetValue {
  value: string
  count: number
}

interface GlobalLogFacetsResponse {
  facets: Record<string, FacetValue[]>
  /** True when a value list was capped or the aggregation timed out. */
  partial: boolean
}

interface AttributeKeysResponse {
  keys: FacetValue[]
}

/** `GET /facets/attrs` — same shape as the label-only facets response but
 * without `partial`: the index answers exactly, it does not sample. */
interface FacetsAttrsResponse {
  facets: Record<string, FacetValue[]>
}

interface HistogramBucket {
  ts: string
  group?: string | null
  count: number
}

interface HistogramResponse {
  buckets: HistogramBucket[]
}

interface AggregateRow {
  keys: string[]
  value: number
  /** Lines that contributed; equals `value` for the `count` metric. */
  lines: number
}

interface AggregateResponse {
  rows: AggregateRow[]
}

interface AnalyticsCapability {
  configured: boolean
  reason?: string | null
  setup_path: string
  example: string
  live_chunks: number
  indexed_chunks: number
}

interface GlobalLogCapabilities {
  analytics: AnalyticsCapability
}

const LEVELS: LogLevel[] = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR']
const MAX_PAGE_SIZE = 1000
const DEFAULT_PAGE_SIZE = 200
const DEFAULT_MAX_PAGES = 20

interface FilterOptions {
  since?: string
  startTime?: string
  endTime?: string
  source?: string
  project?: string[]
  externalService?: string[]
  scope?: string[]
  level?: string[]
  env?: string[]
  service?: string[]
  container?: string[]
  node?: string[]
  deploy?: string
  text?: string
  attr?: string[]
  json?: boolean
}

interface SearchOptions extends FilterOptions {
  limit?: string
  cursor?: string
  all?: boolean
  maxPages?: string
}

interface FacetsOptions extends FilterOptions {
  field?: string[]
  attrKeys?: string
  limit?: string
}

interface AttributesOptions extends FilterOptions {
  limit?: string
}

interface HistogramOptions extends FilterOptions {
  bucketSecs?: string
  groupBy?: string
  maxGroups?: string
}

interface AggregateOptions extends FilterOptions {
  groupBy: string
  metric: string
  limit?: string
}

const collect = (value: string, previous: string[]): string[] => [
  ...previous,
  value,
]

export function registerGlobalLogsCommands(program: Command): void {
  const logs = program
    .command('logs')
    .alias('glogs')
    .description(
      'Search collected logs across every project and database you can access',
    )

  withFilterOptions(
    logs
      .command('search')
      .description('Search log lines across projects (newest first)')
      .option(
        '--limit <n>',
        `Lines per request (default: ${DEFAULT_PAGE_SIZE}, max: ${MAX_PAGE_SIZE})`,
      )
      .option('--cursor <token>', 'Resume from a previous next_cursor')
      .option(
        '--all',
        'Follow next_cursor automatically until the results run out ' +
          `(or --max-pages is reached, default ${DEFAULT_MAX_PAGES} pages)`,
      )
      .option(
        '--max-pages <n>',
        `Page ceiling for --all (default: ${DEFAULT_MAX_PAGES})`,
      ),
  ).action(searchAction)

  withFilterOptions(
    logs
      .command('facets')
      .description(
        'Distinct values and counts per field for the same filter scope',
      )
      .option(
        '--field <name>',
        `Facet field, repeatable: ${FACET_FIELDS.join(', ')} ` +
          '(default: env, service, level, node, deploy)',
        collect,
        [],
      )
      .option(
        '--attr-keys <keys>',
        'Comma-separated label names and/or attr:<name> (e.g. worker,attr:cache) — routes ' +
          'to the attribute-aware endpoint, which requires the ClickHouse line index',
      )
      .option(
        '--limit <n>',
        `Max values per key when using --attr-keys or --attr (default 50, cap ${MAX_ANALYTICS_LIMIT})`,
      ),
  ).action(facetsAction)

  withFilterOptions(
    logs
      .command('attributes')
      .description(
        'Attribute keys observed in the window, most common first ' +
          '(requires the ClickHouse line index)',
      )
      .option(
        '--limit <n>',
        `Max keys returned (default 100, cap ${MAX_ANALYTICS_LIMIT})`,
      ),
  ).action(attributesAction)

  withFilterOptions(
    logs
      .command('histogram')
      .description(
        'Line counts bucketed over time, optionally split by a label or attribute ' +
          '(requires the ClickHouse line index)',
      )
      .option('--bucket-secs <n>', 'Bucket width in seconds (default 60)')
      .option(
        '--group-by <key>',
        'One label name or attr:<name> to split series by',
      )
      .option(
        '--max-groups <n>',
        'Max series before folding the rest into "other" (default 8)',
      ),
  ).action(histogramAction)

  withFilterOptions(
    logs
      .command('aggregate')
      .description(
        'Group-by aggregation over lines (requires the ClickHouse line index)',
      )
      .requiredOption(
        '--group-by <keys>',
        'Comma-separated label names and/or attr:<name>',
      )
      .requiredOption(
        '--metric <spec>',
        'count | count_distinct:<k> | avg:<k> | p50:<k> | p95:<k> | p99:<k> | max:<k> | sum:<k>',
      )
      .option(
        '--limit <n>',
        `Max rows returned (default 50, cap ${MAX_ANALYTICS_LIMIT})`,
      ),
  ).action(aggregateAction)

  logs
    .command('capabilities')
    .description(
      'Whether attribute facets, histograms and aggregates are available on this instance',
    )
    .option('--json', 'Output in JSON format')
    .action(capabilitiesAction)
}

/** Filter flags shared by `logs search` and `logs facets` — same body server-side. */
function withFilterOptions(command: Command): Command {
  return command
    .option(
      '--since <duration>',
      'Relative window ending now, e.g. 30m, 6h, 7d (default: 1h)',
    )
    .option('--start-time <iso>', 'Window start (ISO 8601); overrides --since')
    .option('--end-time <iso>', 'Window end (ISO 8601); defaults to now')
    .option(
      '--source <kind>',
      'collected (default), application, or service',
    )
    .option(
      '--project <id|slug|name>',
      'Restrict to a project, repeatable',
      collect,
      [],
    )
    .option(
      '--external-service <id|name>',
      'Restrict to a managed database/service, repeatable',
      collect,
      [],
    )
    .option(
      '--scope <kind:id>',
      'Explicit resource identity, e.g. application:12, repeatable',
      collect,
      [],
    )
    .option(
      '--level <level>',
      'TRACE|DEBUG|INFO|WARN|ERROR, repeatable',
      collect,
      [],
    )
    .option('--env <name>', 'Environment, repeatable', collect, [])
    .option(
      '--service <name>',
      'Container service label (web, worker, …), repeatable',
      collect,
      [],
    )
    .option('--container <id>', 'Container ID, repeatable', collect, [])
    .option('--node <id>', 'Worker node ID, repeatable', collect, [])
    .option('--deploy <id>', 'Deployment ID')
    .option('--text <substring>', 'Case-insensitive message substring match')
    .option(
      '--attr <pred>',
      'Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, ' +
        '<key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index)',
      collect,
      [],
    )
    .option('--json', 'Output in JSON format')
}

const DURATION = /^(\d+)(m|h|d)$/

export function parseDuration(value: string): number {
  const match = DURATION.exec(value.trim())
  const amount = match?.[1]
  const unit = match?.[2]
  if (!amount || !unit) {
    throw new Error(
      `Invalid duration "${value}". Use a number followed by m, h or d (e.g. 30m, 6h, 7d).`,
    )
  }
  const scale = { m: 60_000, h: 3_600_000, d: 86_400_000 }[
    unit as 'm' | 'h' | 'd'
  ]
  return parseInt(amount, 10) * scale
}

function parsePositiveInt(value: string, label: string): number {
  if (!/^\d+$/.test(value)) throw new Error(`${label} must be a positive integer`)
  const parsed = parseInt(value, 10)
  if (parsed < 1) throw new Error(`${label} must be a positive integer`)
  return parsed
}

/** Turn the shared filter flags into the request body both endpoints accept. */
export function buildFilters(options: FilterOptions): GlobalLogSearchRequest {
  const end = options.endTime ? new Date(options.endTime) : new Date()
  if (Number.isNaN(end.getTime()))
    throw new Error(`Invalid --end-time "${options.endTime}"`)
  let start: Date
  if (options.startTime) {
    start = new Date(options.startTime)
    if (Number.isNaN(start.getTime()))
      throw new Error(`Invalid --start-time "${options.startTime}"`)
  } else {
    start = new Date(end.getTime() - parseDuration(options.since ?? '1h'))
  }
  if (start.getTime() >= end.getTime())
    throw new Error('The window start must be before its end')

  const source = (options.source ?? 'collected') as LogSourceKind
  if (!['collected', 'application', 'service'].includes(source))
    throw new Error(
      `Invalid --source "${options.source}". Use collected, application or service.`,
    )

  const levels = (options.level ?? []).map((level) => {
    const upper = level.toUpperCase() as LogLevel
    if (!LEVELS.includes(upper))
      throw new Error(`Invalid --level "${level}". Use ${LEVELS.join(', ')}.`)
    return upper
  })

  return {
    start_time: start.toISOString(),
    end_time: end.toISOString(),
    source,
    projects: options.project ?? [],
    external_services: options.externalService ?? [],
    scopes: options.scope ?? [],
    levels,
    envs: options.env ?? [],
    services: options.service ?? [],
    container_ids: options.container ?? [],
    node_ids: (options.node ?? []).map((id) => parsePositiveInt(id, '--node')),
    attrs: (options.attr ?? []).map(validateAttrPredicate),
    ...(options.deploy
      ? { deploy_id: parsePositiveInt(options.deploy, '--deploy') }
      : {}),
    ...(options.text ? { text: options.text } : {}),
  }
}

async function searchAction(options: SearchOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const filters = buildFilters(options)
  const pageSize = options.limit
    ? Math.min(parsePositiveInt(options.limit, '--limit'), MAX_PAGE_SIZE)
    : DEFAULT_PAGE_SIZE
  const maxPages = options.all
    ? parsePositiveInt(options.maxPages ?? String(DEFAULT_MAX_PAGES), '--max-pages')
    : 1

  const lines: GlobalLogLine[] = []
  let cursor = options.cursor
  let pages = 0
  let lastPartial = false
  let lastScannedBackTo: string | null = null

  await withSpinner('Searching logs...', async () => {
    do {
      const { data, error } = await client.post({
        url: '/logs/global/search',
        body: { ...filters, page_size: pageSize, ...(cursor ? { cursor } : {}) },
      })
      if (error) throw new Error(getErrorMessage(error))
      const page = data as GlobalLogSearchResponse
      lines.push(...page.lines)
      pages += 1
      lastPartial = page.partial ?? false
      lastScannedBackTo = page.scanned_back_to ?? null
      const next = page.next_cursor ?? undefined
      // A cursor that doesn't advance would loop forever against a misbehaving
      // server; treat it as the end rather than hanging the CLI.
      cursor = next && next !== cursor ? next : undefined
    } while (options.all && cursor && pages < maxPages)
  })

  if (options.json) {
    json({
      lines,
      next_cursor: cursor ?? null,
      partial: lastPartial,
      scanned_back_to: lastScannedBackTo,
      pages,
    })
    return
  }

  newline()
  header(`${icons.info} Logs (${lines.length} ${lines.length === 1 ? 'line' : 'lines'})`)
  keyValue('Window', `${filters.start_time} → ${filters.end_time}`)
  keyValue('Source', filters.source ?? 'collected')

  if (lines.length === 0) {
    newline()
    info('No log lines matched. Widen the window or drop a filter.')
    newline()
    return
  }

  newline()
  const columns: TableColumn<GlobalLogLine>[] = [
    {
      header: 'Time',
      accessor: (line) => new Date(line.timestamp).toISOString(),
      color: (value) => colors.muted(value),
    },
    { header: 'Level', key: 'level', color: levelColor },
    { header: 'Owner', accessor: (line) => line.owner },
    { header: 'Env', accessor: (line) => line.env, color: (v) => colors.muted(v) },
    { header: 'Service', accessor: (line) => line.service },
    {
      header: 'Message',
      accessor: (line) =>
        line.message.length > 100
          ? `${line.message.slice(0, 100)}…`
          : line.message,
    },
  ]
  printTable(lines, columns, { style: 'minimal' })

  newline()
  if (lastPartial && lastScannedBackTo) {
    // Reported rather than glossed: the search budget ran out, not the data.
    // The cursor is still a true prefix, so re-running with it keeps going
    // from exactly where this page stopped.
    warning(
      `Searched back to ${lastScannedBackTo}; run again with --cursor to continue.`,
    )
  }
  if (cursor) {
    info(
      options.all
        ? `Stopped after ${pages} pages (--max-pages). Continue with --cursor ${cursor}`
        : `More results available. Continue with --cursor ${cursor}, or re-run with --all.`,
    )
  } else {
    info('End of results for this window.')
  }
  newline()
}

async function facetsAction(options: FacetsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const filters = buildFilters(options)

  // `--attr-keys` (or a bare `--attr` predicate with no `--field`) routes to
  // the attribute-aware endpoint — the only one that can facet on attr:<name>
  // or filter by attribute predicates — instead of the plain label facets
  // the old `--field` endpoint has always served.
  if (options.attrKeys || (filters.attrs?.length ?? 0) > 0) {
    await facetsAttrsAction(filters, options)
    return
  }

  const fields = (options.field ?? []).map((field) => {
    const lower = field.toLowerCase() as FacetField
    if (!FACET_FIELDS.includes(lower))
      throw new Error(
        `Invalid --field "${field}". Use one of: ${FACET_FIELDS.join(', ')}.`,
      )
    return lower
  })

  const result = await withSpinner('Aggregating facets...', async () => {
    const { data, error } = await client.post({
      url: '/logs/global/facets',
      body: { ...filters, fields },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as GlobalLogFacetsResponse
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Log facets`)
  keyValue('Window', `${filters.start_time} → ${filters.end_time}`)

  const entries = Object.entries(result.facets ?? {}).filter(
    ([, values]) => Array.isArray(values) && values.length > 0,
  )
  if (entries.length === 0) {
    newline()
    info('No values in this window.')
    newline()
    return
  }

  for (const [field, values] of entries) {
    newline()
    header(field)
    printTable(
      values,
      [
        { header: 'Value', accessor: (v: FacetValue) => v.value },
        {
          header: 'Lines',
          accessor: (v: FacetValue) => v.count.toLocaleString(),
          color: (value) => colors.bold(value),
        },
      ],
      { style: 'minimal' },
    )
  }

  newline()
  if (result.partial) {
    // Reported rather than glossed: the point of a facet is that you can trust
    // it to surface values you have never seen.
    warning(
      'Partial: at least one value list was capped or timed out, so it is a ' +
        'prefix of the most common values rather than the complete set. ' +
        'Narrow the window or the filters for an exact list.',
    )
  }
  newline()
}

/** `logs facets --attr-keys …` / `logs facets --attr …`: `GET /facets/attrs`. */
async function facetsAttrsAction(
  filters: GlobalLogSearchRequest,
  options: FacetsOptions,
): Promise<void> {
  const keys = validateGroupKeys(options.attrKeys ?? 'env,service,level,node,deploy')
  const limit = options.limit
    ? Math.min(parsePositiveInt(options.limit, '--limit'), MAX_ANALYTICS_LIMIT)
    : undefined

  const result = await withSpinner('Aggregating attribute facets...', () =>
    fetchAnalytics<FacetsAttrsResponse>('/logs/global/facets/attrs', {
      ...toAnalyticsQuery(filters),
      keys,
      ...(filters.attrs?.length ? { attr: filters.attrs } : {}),
      ...(limit ? { limit } : {}),
    }),
  )

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Log facets (attribute-aware)`)
  keyValue('Window', `${filters.start_time} → ${filters.end_time}`)
  keyValue('Keys', keys)

  const entries = Object.entries(result.facets ?? {}).filter(
    ([, values]) => Array.isArray(values) && values.length > 0,
  )
  if (entries.length === 0) {
    newline()
    info('No values in this window.')
    newline()
    return
  }

  for (const [field, values] of entries) {
    newline()
    header(field)
    printTable(
      values,
      [
        { header: 'Value', accessor: (v: FacetValue) => v.value },
        {
          header: 'Lines',
          accessor: (v: FacetValue) => v.count.toLocaleString(),
          color: (value) => colors.bold(value),
        },
      ],
      { style: 'minimal' },
    )
  }
  newline()
}

async function attributesAction(options: AttributesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const filters = buildFilters(options)
  const limit = options.limit
    ? Math.min(parsePositiveInt(options.limit, '--limit'), MAX_ANALYTICS_LIMIT)
    : undefined

  const result = await withSpinner('Fetching attribute keys...', () =>
    fetchAnalytics<AttributeKeysResponse>('/logs/global/attributes', {
      ...toAnalyticsQuery(filters),
      ...(limit ? { limit } : {}),
    }),
  )

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Attribute keys`)
  keyValue('Window', `${filters.start_time} → ${filters.end_time}`)

  if (result.keys.length === 0) {
    newline()
    info('No attribute keys in this window.')
    newline()
    return
  }

  newline()
  printTable(
    result.keys,
    [
      { header: 'Key', accessor: (v: FacetValue) => v.value },
      {
        header: 'Lines',
        accessor: (v: FacetValue) => v.count.toLocaleString(),
        color: (value) => colors.bold(value),
      },
    ],
    { style: 'minimal' },
  )
  newline()
}

async function histogramAction(options: HistogramOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const filters = buildFilters(options)
  const bucketSecs = options.bucketSecs
    ? parsePositiveInt(options.bucketSecs, '--bucket-secs')
    : undefined
  const maxGroups = options.maxGroups
    ? parsePositiveInt(options.maxGroups, '--max-groups')
    : undefined
  const groupBy = options.groupBy ? validateGroupKey(options.groupBy) : undefined

  const result = await withSpinner('Building histogram...', () =>
    fetchAnalytics<HistogramResponse>('/logs/global/histogram', {
      ...toAnalyticsQuery(filters),
      ...(bucketSecs ? { bucket_secs: bucketSecs } : {}),
      ...(groupBy ? { group_by: groupBy } : {}),
      ...(maxGroups ? { max_groups: maxGroups } : {}),
      ...(filters.attrs?.length ? { attr: filters.attrs } : {}),
    }),
  )

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Log histogram`)
  keyValue('Window', `${filters.start_time} → ${filters.end_time}`)
  if (groupBy) keyValue('Grouped by', groupBy)

  if (result.buckets.length === 0) {
    newline()
    info('No data in this window.')
    newline()
    return
  }

  newline()
  printTable(
    result.buckets,
    [
      {
        header: 'Time',
        accessor: (b: HistogramBucket) => new Date(b.ts).toISOString(),
        color: (value) => colors.muted(value),
      },
      { header: 'Group', accessor: (b: HistogramBucket) => b.group ?? '-' },
      {
        header: 'Count',
        accessor: (b: HistogramBucket) => b.count.toLocaleString(),
        color: (value) => colors.bold(value),
      },
    ],
    { style: 'minimal' },
  )
  newline()
}

async function aggregateAction(options: AggregateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const filters = buildFilters(options)
  const groupBy = validateGroupKeys(options.groupBy)
  const metric = validateMetric(options.metric)
  const limit = options.limit
    ? Math.min(parsePositiveInt(options.limit, '--limit'), MAX_ANALYTICS_LIMIT)
    : undefined

  const result = await withSpinner('Aggregating...', () =>
    fetchAnalytics<AggregateResponse>('/logs/global/aggregate', {
      ...toAnalyticsQuery(filters),
      group_by: groupBy,
      metric,
      ...(limit ? { limit } : {}),
      ...(filters.attrs?.length ? { attr: filters.attrs } : {}),
    }),
  )

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Log aggregate`)
  keyValue('Window', `${filters.start_time} → ${filters.end_time}`)
  keyValue('Group by', groupBy)
  keyValue('Metric', metric)

  if (result.rows.length === 0) {
    newline()
    info('No rows in this window.')
    newline()
    return
  }

  newline()
  printTable(
    result.rows,
    [
      { header: 'Keys', accessor: (r: AggregateRow) => r.keys.join(' / ') },
      {
        header: 'Value',
        accessor: (r: AggregateRow) => r.value.toLocaleString(),
        color: (value) => colors.bold(value),
      },
      { header: 'Lines', accessor: (r: AggregateRow) => r.lines.toLocaleString() },
    ],
    { style: 'minimal' },
  )
  newline()
}

async function capabilitiesAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Checking log analytics capabilities...', () =>
    fetchAnalytics<GlobalLogCapabilities>('/logs/global/capabilities', {}),
  )

  if (options.json) {
    json(result)
    return
  }

  const analytics = result.analytics
  newline()
  header(`${icons.info} Log analytics capabilities`)
  keyValue('Configured', analytics.configured ? 'yes' : 'no')
  keyValue('Live chunks', analytics.live_chunks.toLocaleString())
  keyValue('Indexed chunks', analytics.indexed_chunks.toLocaleString())
  newline()
  if (analytics.configured) {
    info(`Attribute facets, histograms and aggregates are available. Example: ${analytics.example}`)
  } else {
    // Onboards rather than hides: state exactly what's missing, what it
    // would do once configured, and where to fix it (CLAUDE.md).
    warning(analytics.reason ?? 'The ClickHouse line index is not configured.')
    info(`Example once configured: ${analytics.example}`)
    info(`Configure it at ${analytics.setup_path}`)
  }
  newline()
}

function levelColor(level: string): string {
  if (level === 'ERROR') return colors.error(level)
  if (level === 'WARN') return colors.warning(level)
  if (level === 'INFO') return colors.bold(level)
  return colors.muted(level)
}
