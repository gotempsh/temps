import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getApiTimeseries,
  getApiRoutes,
  getApiCallers,
  getApiSummary,
} from '../../api/sdk.gen.js'
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
}

function parseNonNegativeInteger(value: string | undefined, name: string): number | undefined {
  if (value === undefined) return undefined
  if (!/^\d+$/.test(value)) {
    throw new Error(`${name} must be a non-negative integer`)
  }
  return Number.parseInt(value, 10)
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

async function resolveProjectId(projectOption: string | undefined): Promise<{ slug: string; id: number }> {
  const resolved = await requireProjectSlug(projectOption)

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
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
  const environmentId = parseNonNegativeInteger(options.environmentId, 'Environment ID')
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const data = await withSpinner('Fetching API traffic...', async () => {
    const { data, error } = await getApiTimeseries({
      client,
      path: { project_id: projectId },
      query: { environment_id: environmentId, start_date: startDate, end_date: endDate },
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
  console.log(`   ${chalk.bold.white('API Traffic:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`)
  console.log(line)
  newline()

  console.log(`  ${chalk.white('Total Requests')}${' '.repeat(8)}${chalk.bold.green(formatNumber(data?.total_requests ?? 0))}`)
  console.log(`  ${chalk.white('Total Errors')}${' '.repeat(10)}${chalk.bold.green(formatNumber(data?.total_errors ?? 0))} ${chalk.gray(`(${formatPercent(data?.overall_error_rate ?? 0)})`)}`)
  console.log(`  ${chalk.white('Avg Latency')}${' '.repeat(11)}${chalk.bold.green(formatMs(data?.overall_avg_latency_ms))}`)
  console.log(`  ${chalk.white('Bucket Interval')}${' '.repeat(7)}${chalk.gray(data?.bucket_interval ?? 'n/a')}`)

  const points = data?.points ?? []
  if (points.length > 0) {
    newline()
    console.log(`  ${chalk.bold.white('Timeseries')}`)
    console.log(`  ${chalk.gray('─'.repeat(76))}`)
    console.log(
      `  ${chalk.gray('Time'.padEnd(22))}${chalk.gray('Requests'.padStart(10))}${chalk.gray('Errors'.padStart(9))}${chalk.gray('Error %'.padStart(9))}${chalk.gray('p95'.padStart(9))}${chalk.gray('p99'.padStart(9))}`
    )
    points.forEach((p) => {
      console.log(
        `  ${p.timestamp.padEnd(22)}${formatNumber(p.request_count).padStart(10)}${formatNumber(p.error_count).padStart(9)}${formatPercent(p.error_rate).padStart(9)}${formatMs(p.p95_latency_ms).padStart(9)}${formatMs(p.p99_latency_ms).padStart(9)}`
      )
    })
  }

  newline()
  console.log(line)
  newline()
}

export async function apiRoutes(options: ApiTrafficLimitOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const limit = options.limit ? parseInt(options.limit, 10) : 20
  const offset = parseNonNegativeInteger(options.offset, 'Offset') ?? 0
  const environmentId = parseNonNegativeInteger(options.environmentId, 'Environment ID')
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const data = await withSpinner('Fetching top routes...', async () => {
    const { data, error } = await getApiRoutes({
      client,
      path: { project_id: projectId },
      query: { environment_id: environmentId, start_date: startDate, end_date: endDate, limit, offset },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }

  const line = chalk.cyan('━'.repeat(64))
  const routes = data?.routes ?? []

  newline()
  console.log(line)
  console.log(`   ${chalk.bold.white('Top API Routes:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`)
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
    `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Method'.padEnd(8))}${chalk.gray('Path'.padEnd(38))}${chalk.gray('Requests'.padStart(10))}${chalk.gray('Avg'.padStart(9))}${chalk.gray('Err %'.padStart(8))}`
  )
  console.log(`  ${chalk.gray('─'.repeat(76))}`)

  routes.forEach((r, i) => {
    const path = r.path.length > 36 ? r.path.slice(0, 33) + '...' : r.path
    console.log(
      `  ${chalk.gray(String(offset + i + 1).padEnd(4))}${chalk.cyan(r.method.padEnd(8))}${chalk.white(path.padEnd(38))}${formatNumber(r.request_count).padStart(10)}${formatMs(r.avg_latency_ms).padStart(9)}${formatPercent(r.error_rate).padStart(8)}`
    )
  })

  newline()
  console.log(`  ${chalk.gray('Total distinct routes:')} ${formatNumber(data?.total_routes ?? 0)}`)
  newline()
  console.log(line)
  newline()
}

export async function apiCallers(options: ApiTrafficLimitOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const limit = options.limit ? parseInt(options.limit, 10) : 20
  const offset = parseNonNegativeInteger(options.offset, 'Offset') ?? 0
  const environmentId = parseNonNegativeInteger(options.environmentId, 'Environment ID')
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const data = await withSpinner('Fetching top callers...', async () => {
    const { data, error } = await getApiCallers({
      client,
      path: { project_id: projectId },
      query: { environment_id: environmentId, start_date: startDate, end_date: endDate, limit, offset },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    jsonOut({ project: slug, period, ...data })
    return
  }

  const line = chalk.cyan('━'.repeat(64))
  const callers = data?.callers ?? []

  newline()
  console.log(line)
  console.log(`   ${chalk.bold.white('Top API Callers:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`)
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
    `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Client IP'.padEnd(24))}${chalk.gray('Requests'.padStart(10))}${chalk.gray('Err %'.padStart(8))}${chalk.gray('Last Seen'.padStart(26))}`
  )
  console.log(`  ${chalk.gray('─'.repeat(78))}`)

  callers.forEach((c, i) => {
    console.log(
      `  ${chalk.gray(String(offset + i + 1).padEnd(4))}${chalk.white(c.client_ip.padEnd(24))}${formatNumber(c.request_count).padStart(10)}${formatPercent(c.error_rate).padStart(8)}${c.last_seen.padStart(26)}`
    )
  })

  newline()
  console.log(`  ${chalk.gray('Total distinct callers:')} ${formatNumber(data?.total_callers ?? 0)}`)
  newline()
  console.log(line)
  newline()
}

export async function apiSummary(options: ApiTrafficOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const environmentId = parseNonNegativeInteger(options.environmentId, 'Environment ID')
  const { startDate, endDate, label } = parsePeriod(period)
  const { slug, id: projectId } = await resolveProjectId(options.project)

  const data = await withSpinner('Generating AI summary...', async () => {
    const { data, error } = await getApiSummary({
      client,
      path: { project_id: projectId },
      query: { environment_id: environmentId, start_date: startDate, end_date: endDate },
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
  console.log(`   ${chalk.bold.white('AI Traffic Summary:')} ${chalk.bold.cyan(slug)} ${chalk.gray(`(${label})`)}`)
  console.log(line)
  newline()

  if (!data?.summary) {
    console.log(`  ${chalk.yellow('No summary available.')}`)
    if (data?.unavailable_reason) {
      console.log(`  ${chalk.gray(data.unavailable_reason)}`)
    }
    if (!data?.enabled) {
      console.log(
        `  ${chalk.gray('Enable it in project settings (AI Assistance) to generate summaries here.')}`
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
    data.summary.findings.forEach((f) => console.log(`  ${chalk.gray('•')} ${f}`))
  }

  if (data.summary.anomalies.length > 0) {
    newline()
    console.log(`  ${chalk.bold.yellow('Anomalies')}`)
    data.summary.anomalies.forEach((a) => console.log(`  ${chalk.yellow('•')} ${a}`))
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
