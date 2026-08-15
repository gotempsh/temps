import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  aggregateApiTraffic,
  getProjectBySlug,
  getApiTimeseries,
  getApiSummary,
} from '../../api/sdk.gen.js'
import type {
  TrafficAggregationRequest,
  TrafficAggregationResponse,
  TrafficAggregationRow,
  TrafficDimension,
  TrafficFilter,
  TrafficFilterOperator,
  TrafficMetric,
  TrafficSortDirection,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, colors, info } from '../../ui/output.js'
import { parsePeriod } from './period.js'

interface ApiTrafficOptions {
  project?: string
  period?: string
  environmentId?: string
  json?: boolean
}

interface ApiTrafficLimitOptions extends ApiTrafficOptions {
  limit?: string
  offset?: string
  sortBy?: string
  order?: string
  includeSynthetic?: boolean
}

export interface ApiTrafficQueryOptions extends ApiTrafficOptions {
  groupBy?: string
  metrics: string
  filters?: string[]
  sortBy?: string
  order?: string
  page?: string
  limit?: string
  includeSynthetic?: boolean
}

const TRAFFIC_DIMENSIONS = new Set<TrafficDimension>([
  'client_ip',
  'method',
  'path',
  'host',
  'status_code',
  'status_class',
  'environment_id',
  'deployment_id',
  'request_source',
  'is_bot',
  'browser',
  'operating_system',
  'device_type',
  'cache_status',
])
const TRAFFIC_METRICS = new Set<TrafficMetric>([
  'requests',
  'errors',
  'error_rate',
  'latency_avg',
  'latency_min',
  'latency_max',
  'latency_p50',
  'latency_p95',
  'latency_p99',
  'unique_ips',
  'unique_paths',
  'last_seen',
])

function parseCsv<T extends string>(
  raw: string,
  allowed: Set<T>,
  label: string,
  allowEmpty = false,
): T[] {
  const values = raw
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean)
  const invalid = values.find((value) => !allowed.has(value as T))
  if (invalid) throw new Error(`Unknown ${label} "${invalid}"`)
  if (values.length === 0 && !allowEmpty)
    throw new Error(`${label} cannot be empty`)
  return values as T[]
}

function parseFilter(raw: string): {
  dimension: TrafficDimension
  operator: TrafficFilterOperator
  values: string[]
} {
  const first = raw.indexOf(':')
  const second = raw.indexOf(':', first + 1)
  if (first <= 0 || second <= first + 1) {
    throw new Error(
      `Filter must use dimension:operator:value, received "${raw}"`,
    )
  }
  const dimension = raw.slice(0, first) as TrafficDimension
  const operator = raw.slice(first + 1, second) as TrafficFilterOperator
  const value = raw.slice(second + 1)
  if (!TRAFFIC_DIMENSIONS.has(dimension))
    throw new Error(`Unknown filter dimension "${dimension}"`)
  if (!['eq', 'not_eq', 'contains', 'starts_with', 'in'].includes(operator)) {
    throw new Error(`Unknown filter operator "${operator}"`)
  }
  return {
    dimension,
    operator,
    values: operator === 'in' ? value.split(',') : [value],
  }
}

function dimension(row: TrafficAggregationRow, name: TrafficDimension): string {
  return row.dimensions.find((item) => item.dimension === name)?.value ?? 'n/a'
}

async function aggregateTraffic(
  projectId: number,
  body: TrafficAggregationRequest,
): Promise<TrafficAggregationResponse> {
  const { data, error } = await aggregateApiTraffic({
    client,
    path: { project_id: projectId },
    body,
  })
  if (error || !data) throw new Error(getErrorMessage(error))
  return data
}

const TRAFFIC_STORAGE_PAGE_SIZE = 100

export function trafficOffsetPlanForTest(offset: number): {
  page: number
  skip: number
} {
  return {
    page: Math.floor(offset / TRAFFIC_STORAGE_PAGE_SIZE) + 1,
    skip: offset % TRAFFIC_STORAGE_PAGE_SIZE,
  }
}

async function aggregateTrafficAtOffset(
  projectId: number,
  body: TrafficAggregationRequest,
  limit: number,
  offset: number,
): Promise<TrafficAggregationResponse> {
  const plan = trafficOffsetPlanForTest(offset)
  const first = await aggregateTraffic(projectId, {
    ...body,
    page: plan.page,
    page_size: TRAFFIC_STORAGE_PAGE_SIZE,
  })
  const rows = first.rows.slice(plan.skip, plan.skip + limit)

  if (rows.length < limit && plan.page < first.total_pages) {
    const next = await aggregateTraffic(projectId, {
      ...body,
      page: plan.page + 1,
      page_size: TRAFFIC_STORAGE_PAGE_SIZE,
    })
    rows.push(...next.rows.slice(0, limit - rows.length))
  }

  return {
    ...first,
    rows,
    page: Math.floor(offset / limit) + 1,
    page_size: limit,
    total_pages: Math.ceil(first.total_groups / limit),
  }
}

function parseNonNegativeInteger(
  value: string | undefined,
  name: string,
): number | undefined {
  if (value === undefined) return undefined
  if (!/^\d+$/.test(value)) {
    throw new Error(`${name} must be a non-negative integer`)
  }
  return Number.parseInt(value, 10)
}

function parsePageSize(value: string | undefined): number {
  const parsed = parseNonNegativeInteger(value, 'Limit') ?? 20
  if (parsed < 1 || parsed > 100) {
    throw new Error('Limit must be between 1 and 100')
  }
  return parsed
}

function parseSortDirection(value: string | undefined): TrafficSortDirection {
  if (value === undefined) return 'desc'
  if (value !== 'asc' && value !== 'desc') {
    throw new Error('Order must be asc or desc')
  }
  return value
}

function formatNumber(n: number): string {
  return n.toLocaleString('en-US')
}

function formatMs(ms: number | null | undefined): string {
  return ms == null ? 'n/a' : `${ms.toFixed(0)}ms`
}

function formatPercent(rate: number): string {
  return `${(rate * 100).toFixed(1)}%`
}

// Exported under a `ForTest` alias so the public surface of this module still
// reads as "commands", not a formatting-utils grab bag.
export const formatMsForTest = formatMs
export const formatPercentForTest = formatPercent
export const parseNonNegativeIntegerForTest = parseNonNegativeInteger
export const parsePageSizeForTest = parsePageSize
export const parseTrafficFilterForTest = parseFilter

async function resolveProjectId(
  projectOption: string | undefined,
): Promise<{ slug: string; id: number }> {
  const resolved = await requireProjectSlug(projectOption)

  if (resolved.source !== 'flag') {
    info(
      `Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`,
    )
  }

  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })

  if (projectError || !projectData) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }

  return { slug: resolved.slug, id: projectData.id }
}

export async function apiOverview(options: ApiTrafficOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const environmentId = parseNonNegativeInteger(
    options.environmentId,
    'Environment ID',
  )
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const data = await withSpinner('Fetching API traffic...', async () => {
    const { data, error } = await getApiTimeseries({
      client,
      path: { project_id: projectId },
      query: {
        environment_id: environmentId,
        start_date: startDate,
        end_date: endDate,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }

  const line = chalk.cyan('━'.repeat(64))

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('API Traffic:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`,
  )
  console.log(line)
  newline()

  console.log(
    `  ${chalk.white('Total Requests')}${' '.repeat(8)}${chalk.bold.green(formatNumber(data?.total_requests ?? 0))}`,
  )
  console.log(
    `  ${chalk.white('Total Errors')}${' '.repeat(10)}${chalk.bold.green(formatNumber(data?.total_errors ?? 0))} ${chalk.gray(`(${formatPercent(data?.overall_error_rate ?? 0)})`)}`,
  )
  console.log(
    `  ${chalk.white('Avg Latency')}${' '.repeat(11)}${chalk.bold.green(formatMs(data?.overall_avg_latency_ms))}`,
  )
  console.log(
    `  ${chalk.white('Bucket Interval')}${' '.repeat(7)}${chalk.gray(data?.bucket_interval ?? 'n/a')}`,
  )

  const points = data?.points ?? []
  if (points.length > 0) {
    newline()
    console.log(`  ${chalk.bold.white('Timeseries')}`)
    console.log(`  ${chalk.gray('─'.repeat(76))}`)
    console.log(
      `  ${chalk.gray('Time'.padEnd(22))}${chalk.gray('Requests'.padStart(10))}${chalk.gray('Errors'.padStart(9))}${chalk.gray('Error %'.padStart(9))}${chalk.gray('p95'.padStart(9))}${chalk.gray('p99'.padStart(9))}`,
    )
    points.forEach((p) => {
      console.log(
        `  ${p.timestamp.padEnd(22)}${formatNumber(p.request_count).padStart(10)}${formatNumber(p.error_count).padStart(9)}${formatPercent(p.error_rate).padStart(9)}${formatMs(p.p95_latency_ms).padStart(9)}${formatMs(p.p99_latency_ms).padStart(9)}`,
      )
    })
  }

  newline()
  console.log(line)
  newline()
}

export async function apiRoutes(
  options: ApiTrafficLimitOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const limit = parsePageSize(options.limit)
  const offset = parseNonNegativeInteger(options.offset, 'Offset') ?? 0
  const environmentId = parseNonNegativeInteger(
    options.environmentId,
    'Environment ID',
  )
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const sortBy = (options.sortBy ?? 'requests') as TrafficMetric
  if (!['requests', 'latency_avg', 'error_rate'].includes(sortBy)) {
    throw new Error('Route sort must be requests, latency_avg, or error_rate')
  }
  const data = await withSpinner('Fetching top routes...', () =>
    aggregateTrafficAtOffset(
      projectId,
      {
        start_time: startDate,
        end_time: endDate,
        environment_id: environmentId,
        dimensions: ['method', 'path'],
        metrics: [
          'requests',
          'errors',
          'error_rate',
          'latency_avg',
          'latency_min',
          'latency_max',
          'last_seen',
        ],
        filters: [],
        order_by: [
          {
            field: { kind: 'metric', field: sortBy },
            direction: parseSortDirection(options.order),
          },
        ],
        include_synthetic: options.includeSynthetic ?? false,
        page: 1,
        page_size: TRAFFIC_STORAGE_PAGE_SIZE,
      },
      limit,
      offset,
    ),
  )

  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }

  const line = chalk.cyan('━'.repeat(64))
  const routes = data.rows

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('Top API Routes:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`,
  )
  console.log(line)
  newline()

  if (routes.length === 0) {
    console.log(`  ${chalk.gray('No API traffic for this period.')}`)
    newline()
    console.log(line)
    newline()
    return
  }

  console.log(
    `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Method'.padEnd(8))}${chalk.gray('Path'.padEnd(30))}${chalk.gray('Requests'.padStart(10))}${chalk.gray('Min'.padStart(8))}${chalk.gray('Avg'.padStart(8))}${chalk.gray('Max'.padStart(8))}${chalk.gray('Err %'.padStart(8))}`,
  )
  console.log(`  ${chalk.gray('─'.repeat(76))}`)

  routes.forEach((r, i) => {
    const rawPath = dimension(r, 'path')
    const path = rawPath.length > 28 ? rawPath.slice(0, 25) + '...' : rawPath
    console.log(
      `  ${chalk.gray(String(offset + i + 1).padEnd(4))}${chalk.cyan(dimension(r, 'method').padEnd(8))}${chalk.white(path.padEnd(30))}${formatNumber(r.metrics.requests ?? 0).padStart(10)}${formatMs(r.metrics.latency_min_ms).padStart(8)}${formatMs(r.metrics.latency_avg_ms).padStart(8)}${formatMs(r.metrics.latency_max_ms).padStart(8)}${formatPercent(r.metrics.error_rate ?? 0).padStart(8)}`,
    )
  })

  newline()
  console.log(
    `  ${chalk.gray('Total distinct routes:')} ${formatNumber(data.total_groups)}`,
  )
  console.log(
    `  ${chalk.gray('Temps monitor traffic:')} ${data.synthetic_excluded ? 'excluded' : 'included'}`,
  )
  newline()
  console.log(line)
  newline()
}

export async function apiCallers(
  options: ApiTrafficLimitOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const limit = parsePageSize(options.limit)
  const offset = parseNonNegativeInteger(options.offset, 'Offset') ?? 0
  const environmentId = parseNonNegativeInteger(
    options.environmentId,
    'Environment ID',
  )
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const sortBy = (options.sortBy ?? 'requests') as TrafficMetric
  if (!['requests', 'error_rate'].includes(sortBy)) {
    throw new Error('Caller sort must be requests or error_rate')
  }
  const data = await withSpinner('Fetching top callers...', () =>
    aggregateTrafficAtOffset(
      projectId,
      {
        start_time: startDate,
        end_time: endDate,
        environment_id: environmentId,
        dimensions: ['client_ip'],
        metrics: [
          'requests',
          'errors',
          'error_rate',
          'latency_avg',
          'unique_paths',
          'last_seen',
        ],
        filters: [],
        order_by: [
          {
            field: { kind: 'metric', field: sortBy },
            direction: parseSortDirection(options.order),
          },
        ],
        include_synthetic: options.includeSynthetic ?? false,
        page: 1,
        page_size: TRAFFIC_STORAGE_PAGE_SIZE,
      },
      limit,
      offset,
    ),
  )

  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }

  const line = chalk.cyan('━'.repeat(64))
  const callers = data.rows

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('Top API Callers:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`,
  )
  console.log(line)
  newline()

  if (callers.length === 0) {
    console.log(`  ${chalk.gray('No API traffic for this period.')}`)
    newline()
    console.log(line)
    newline()
    return
  }

  console.log(
    `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Client IP'.padEnd(24))}${chalk.gray('Requests'.padStart(10))}${chalk.gray('Paths'.padStart(8))}${chalk.gray('Err %'.padStart(8))}${chalk.gray('Last Seen'.padStart(26))}`,
  )
  console.log(`  ${chalk.gray('─'.repeat(78))}`)

  callers.forEach((c, i) => {
    console.log(
      `  ${chalk.gray(String(offset + i + 1).padEnd(4))}${chalk.white(dimension(c, 'client_ip').padEnd(24))}${formatNumber(c.metrics.requests ?? 0).padStart(10)}${formatNumber(c.metrics.unique_paths ?? 0).padStart(8)}${formatPercent(c.metrics.error_rate ?? 0).padStart(8)}${(c.metrics.last_seen ?? 'n/a').padStart(26)}`,
    )
  })

  newline()
  console.log(
    `  ${chalk.gray('Total distinct callers:')} ${formatNumber(data.total_groups)}`,
  )
  console.log(
    `  ${chalk.gray('Temps monitor traffic:')} ${data.synthetic_excluded ? 'excluded' : 'included'}`,
  )
  newline()
  console.log(line)
  newline()
}

function printAggregation(
  title: string,
  slug: string,
  label: string,
  data: TrafficAggregationResponse,
): void {
  const line = chalk.cyan('━'.repeat(88))
  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white(title)} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`,
  )
  console.log(line)
  newline()
  if (data.rows.length === 0) {
    console.log(`  ${chalk.gray('No API traffic matched this query.')}`)
  } else {
    for (const [index, row] of data.rows.entries()) {
      const dimensions = row.dimensions
        .map((item) => `${item.dimension}=${item.value ?? 'n/a'}`)
        .join(' · ')
      const metrics = Object.entries(row.metrics)
        .filter(([, value]) => value !== null)
        .map(
          ([name, value]) =>
            `${name}=${typeof value === 'number' ? value.toLocaleString('en-US') : value}`,
        )
        .join(' · ')
      console.log(
        `  ${chalk.gray(String((data.page - 1) * data.page_size + index + 1).padStart(3))}  ${chalk.white(dimensions)}`,
      )
      console.log(`       ${chalk.gray(metrics)}`)
    }
  }
  newline()
  console.log(
    `  ${chalk.gray('Groups:')} ${formatNumber(data.total_groups)}  ${chalk.gray('Page:')} ${data.page}/${Math.max(1, data.total_pages)}`,
  )
  console.log(
    `  ${chalk.gray('Temps monitor traffic:')} ${data.synthetic_excluded ? 'excluded' : 'included'}`,
  )
  newline()
  console.log(line)
  newline()
}

async function runTrafficAggregation(
  options: ApiTrafficOptions & {
    page?: string
    limit?: string
    includeSynthetic?: boolean
  },
  dimensions: TrafficDimension[],
  metrics: TrafficMetric[],
  filters: TrafficFilter[],
  sortBy: TrafficDimension | TrafficMetric,
  order: string | undefined,
  title: string,
): Promise<void> {
  await requireAuth()
  await setupClient()
  const period = options.period ?? '24h'
  const environmentId = parseNonNegativeInteger(
    options.environmentId,
    'Environment ID',
  )
  const page = parseNonNegativeInteger(options.page, 'Page') ?? 1
  const pageSize = parsePageSize(options.limit)
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)
  const orderField = dimensions.includes(sortBy as TrafficDimension)
    ? { kind: 'dimension' as const, field: sortBy as TrafficDimension }
    : { kind: 'metric' as const, field: sortBy as TrafficMetric }
  const data = await withSpinner('Aggregating API traffic...', () =>
    aggregateTraffic(projectId, {
      start_time: startDate,
      end_time: endDate,
      environment_id: environmentId,
      dimensions,
      metrics,
      filters,
      order_by: [{ field: orderField, direction: parseSortDirection(order) }],
      include_synthetic: options.includeSynthetic ?? false,
      page,
      page_size: pageSize,
    }),
  )
  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }
  printAggregation(title, slug, label, data)
}

export async function apiIp(
  ip: string,
  options: ApiTrafficLimitOptions & { page?: string },
): Promise<void> {
  await runTrafficAggregation(
    options,
    ['method', 'path'],
    [
      'requests',
      'errors',
      'error_rate',
      'latency_avg',
      'latency_min',
      'latency_max',
      'latency_p95',
      'last_seen',
    ],
    [{ dimension: 'client_ip', operator: 'eq', values: [ip] }],
    (options.sortBy as TrafficMetric | undefined) ?? 'requests',
    options.order,
    `Routes called by ${ip}:`,
  )
}

export async function apiPath(
  path: string,
  options: ApiTrafficLimitOptions & { page?: string },
): Promise<void> {
  await runTrafficAggregation(
    options,
    ['client_ip'],
    [
      'requests',
      'errors',
      'error_rate',
      'latency_avg',
      'latency_min',
      'latency_max',
      'latency_p95',
      'last_seen',
    ],
    [{ dimension: 'path', operator: 'eq', values: [path] }],
    (options.sortBy as TrafficMetric | undefined) ?? 'requests',
    options.order,
    `IPs calling ${path}:`,
  )
}

export async function apiQuery(options: ApiTrafficQueryOptions): Promise<void> {
  const dimensions = parseCsv(
    options.groupBy ?? '',
    TRAFFIC_DIMENSIONS,
    'dimension',
    true,
  )
  const metrics = parseCsv(options.metrics, TRAFFIC_METRICS, 'metric')
  const defaultSort: TrafficDimension | TrafficMetric = metrics.includes(
    'requests',
  )
    ? 'requests'
    : (dimensions[0] ?? metrics[0]!)
  const sortBy = options.sortBy
    ? [...TRAFFIC_DIMENSIONS, ...TRAFFIC_METRICS].includes(
        options.sortBy as never,
      )
      ? (options.sortBy as TrafficDimension | TrafficMetric)
      : (() => {
          throw new Error(`Unknown sort field "${options.sortBy}"`)
        })()
    : defaultSort
  await runTrafficAggregation(
    options,
    dimensions,
    metrics,
    (options.filters ?? []).map(parseFilter),
    sortBy,
    options.order,
    'API traffic query:',
  )
}

export async function apiSummary(options: ApiTrafficOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const environmentId = parseNonNegativeInteger(
    options.environmentId,
    'Environment ID',
  )
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const data = await withSpinner('Generating AI summary...', async () => {
    const { data, error } = await getApiSummary({
      client,
      path: { project_id: projectId },
      query: {
        environment_id: environmentId,
        start_date: startDate,
        end_date: endDate,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }

  const line = chalk.cyan('━'.repeat(64))

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('AI Traffic Summary:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`,
  )
  console.log(line)
  newline()

  if (!data?.summary) {
    console.log(`  ${chalk.yellow('No summary available.')}`)
    if (data?.unavailable_reason) {
      console.log(`  ${chalk.gray(data.unavailable_reason)}`)
    }
    if (!data?.enabled) {
      console.log(
        `  ${chalk.gray('Enable it in project settings (AI Assistance) to generate summaries here.')}`,
      )
    }
    newline()
    console.log(line)
    newline()
    return
  }

  console.log(`  ${chalk.bold.white(data.summary.headline)}`)

  if (data.summary.findings.length > 0) {
    newline()
    console.log(`  ${chalk.bold.white('Findings')}`)
    data.summary.findings.forEach((f) =>
      console.log(`  ${chalk.gray('•')} ${f}`),
    )
  }

  if (data.summary.anomalies.length > 0) {
    newline()
    console.log(`  ${chalk.bold.yellow('Anomalies')}`)
    data.summary.anomalies.forEach((a) =>
      console.log(`  ${chalk.yellow('•')} ${a}`),
    )
  }

  if (data.summary.recommendation) {
    newline()
    console.log(`  ${chalk.bold.white('Recommendation')}`)
    console.log(`  ${data.summary.recommendation}`)
  }

  newline()
  console.log(line)
  newline()
}
