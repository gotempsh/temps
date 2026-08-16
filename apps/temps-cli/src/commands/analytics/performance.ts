import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import {
  getGroupedPageMetrics,
  getMetricsOverTime,
  getPerformanceMetrics,
  getProjectBySlug,
} from '../../api/sdk.gen.js'
import type {
  GetPerformanceMetricsData,
  GroupedPageMetric,
  GroupedPageMetricsResponse,
  MetricsOverTimeResponse,
  PerformanceMetricsResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { colors, info, json as jsonOut, newline } from '../../ui/output.js'
import { sanitizeTerminalText } from '../../ui/terminal.js'
import { parsePeriod } from './period.js'

const DEVICES = ['desktop', 'mobile'] as const
const MAX_BACKEND_ID = 2_147_483_647
const RFC3339_DATE_TIME =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:Z|[+-](\d{2}):(\d{2}))$/
const GROUP_BY_DIMENSIONS = [
  'path',
  'country',
  'region',
  'city',
  'device_type',
  'browser',
  'operating_system',
] as const

type Device = (typeof DEVICES)[number]
type GroupByDimension = (typeof GROUP_BY_DIMENSIONS)[number]
type PerformanceQuery = GetPerformanceMetricsData['query']

export interface PerformanceOptions {
  project?: string
  period?: string
  startDate?: string
  endDate?: string
  environmentId?: string
  deploymentId?: string
  device?: string
  includeBots?: boolean
  groupBy?: string
  path?: string
  country?: string
  region?: string
  city?: string
  browser?: string
  os?: string
  json?: boolean
}

interface ResolvedWindow {
  startDate: string
  endDate: string
  label: string
  period?: string
}

interface MetricDefinition {
  key: 'lcp' | 'inp' | 'cls' | 'fcp' | 'ttfb' | 'fid'
  label: string
  unit: 'ms' | 'score'
}

interface PerformanceRequestResult<T> {
  data?: T
  error?: unknown
}

export interface PerformanceApi {
  getSummary(query: PerformanceQuery): Promise<PerformanceRequestResult<PerformanceMetricsResponse>>
  getTimeline(query: PerformanceQuery): Promise<PerformanceRequestResult<MetricsOverTimeResponse>>
  getBreakdown(
    query: PerformanceQuery & { group_by: GroupByDimension }
  ): Promise<PerformanceRequestResult<GroupedPageMetricsResponse>>
}

const METRICS: MetricDefinition[] = [
  { key: 'lcp', label: 'LCP', unit: 'ms' },
  { key: 'inp', label: 'INP', unit: 'ms' },
  { key: 'cls', label: 'CLS', unit: 'score' },
  { key: 'fcp', label: 'FCP', unit: 'ms' },
  { key: 'ttfb', label: 'TTFB', unit: 'ms' },
  { key: 'fid', label: 'FID', unit: 'ms' },
]

function parsePositiveInteger(value: string | undefined, label: string): number | undefined {
  if (value === undefined) return undefined
  if (!/^[1-9]\d*$/.test(value)) {
    throw new Error(`${label} must be a positive integer`)
  }
  const parsed = Number.parseInt(value, 10)
  if (!Number.isSafeInteger(parsed) || parsed > MAX_BACKEND_ID) {
    throw new Error(`${label} must be between 1 and ${MAX_BACKEND_ID}`)
  }
  return parsed
}

function parseDevice(value: string | undefined): Device | undefined {
  if (value === undefined) return undefined
  if (!DEVICES.includes(value as Device)) {
    throw new Error(`Device must be one of: ${DEVICES.join(', ')}`)
  }
  return value as Device
}

function parseGroupBy(value: string | undefined): GroupByDimension | undefined {
  if (value === undefined) return undefined
  if (!GROUP_BY_DIMENSIONS.includes(value as GroupByDimension)) {
    throw new Error(`Group by must be one of: ${GROUP_BY_DIMENSIONS.join(', ')}`)
  }
  return value as GroupByDimension
}

function parseDate(value: string, label: string): string {
  const match = RFC3339_DATE_TIME.exec(value)
  if (!match) {
    throw new Error(`${label} must be a valid RFC 3339 date`)
  }

  const [, yearText, monthText, dayText, hourText, minuteText, secondText, offsetHourText, offsetMinuteText] =
    match
  const year = Number(yearText)
  const month = Number(monthText)
  const day = Number(dayText)
  const hour = Number(hourText)
  const minute = Number(minuteText)
  const second = Number(secondText)
  const offsetHour = offsetHourText === undefined ? 0 : Number(offsetHourText)
  const offsetMinute = offsetMinuteText === undefined ? 0 : Number(offsetMinuteText)
  const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0)
  const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
  const validComponents =
    month >= 1 &&
    month <= 12 &&
    day >= 1 &&
    day <= (daysInMonth[month - 1] ?? 0) &&
    hour <= 23 &&
    minute <= 59 &&
    second <= 59 &&
    offsetHour <= 23 &&
    offsetMinute <= 59
  if (!validComponents) {
    throw new Error(`${label} must be a valid RFC 3339 date`)
  }

  const timestamp = Date.parse(value)
  if (Number.isNaN(timestamp)) {
    throw new Error(`${label} must be a valid RFC 3339 date`)
  }
  return new Date(timestamp).toISOString()
}

const defaultPerformanceApi: PerformanceApi = {
  getSummary: (query) => getPerformanceMetrics({ client, query }),
  getTimeline: (query) => getMetricsOverTime({ client, query }),
  getBreakdown: (query) => getGroupedPageMetrics({ client, query }),
}

export async function fetchPerformanceData(
  query: PerformanceQuery,
  groupBy?: GroupByDimension,
  api: PerformanceApi = defaultPerformanceApi
): Promise<{
  summary?: PerformanceMetricsResponse
  timeline?: MetricsOverTimeResponse
  breakdown?: GroupedPageMetricsResponse
}> {
  const [summaryResult, timelineResult, breakdownResult] = await Promise.all([
    api.getSummary(query),
    api.getTimeline(query),
    groupBy ? api.getBreakdown({ ...query, group_by: groupBy }) : Promise.resolve(undefined),
  ])

  if (summaryResult.error) {
    throw new Error(sanitizeTerminalText(getErrorMessage(summaryResult.error)))
  }
  if (timelineResult.error) {
    throw new Error(sanitizeTerminalText(getErrorMessage(timelineResult.error)))
  }
  if (breakdownResult?.error) {
    throw new Error(sanitizeTerminalText(getErrorMessage(breakdownResult.error)))
  }

  return {
    summary: summaryResult.data,
    timeline: timelineResult.data,
    breakdown: breakdownResult?.data,
  }
}

export function resolvePerformanceWindow(options: PerformanceOptions): ResolvedWindow {
  const hasStart = options.startDate !== undefined
  const hasEnd = options.endDate !== undefined

  if (hasStart !== hasEnd) {
    throw new Error('--start-date and --end-date must be provided together')
  }

  if (hasStart && hasEnd) {
    const startValue = options.startDate ?? ''
    const endValue = options.endDate ?? ''
    const startDate = parseDate(startValue, 'Start date')
    const endDate = parseDate(endValue, 'End date')
    if (Date.parse(startDate) >= Date.parse(endDate)) {
      throw new Error('Start date must be earlier than end date')
    }
    return { startDate, endDate, label: `${startDate} to ${endDate}` }
  }

  const period = options.period ?? '7d'
  const window = parsePeriod(period)
  return { ...window, period }
}

export function buildPerformanceQuery(
  projectId: number,
  window: ResolvedWindow,
  options: PerformanceOptions
): PerformanceQuery {
  return {
    project_id: projectId,
    start_date: window.startDate,
    end_date: window.endDate,
    environment_id: parsePositiveInteger(options.environmentId, 'Environment ID'),
    deployment_id: parsePositiveInteger(options.deploymentId, 'Deployment ID'),
    device_type: parseDevice(options.device),
    include_bots: options.includeBots ?? false,
    filter_path: options.path,
    filter_country: options.country,
    filter_region: options.region,
    filter_city: options.city,
    filter_browser: options.browser,
    filter_operating_system: options.os,
  }
}

function formatMetric(value: number | null | undefined, unit: MetricDefinition['unit']): string {
  if (value === null || value === undefined) return 'n/a'
  return unit === 'score' ? value.toFixed(3) : `${Math.round(value)}ms`
}

function metricValue(
  data: PerformanceMetricsResponse,
  metric: MetricDefinition['key'],
  percentile?: 'p75' | 'p90' | 'p95' | 'p99'
): number | null | undefined {
  const key = percentile ? `${metric}_${percentile}` : metric
  return data[key as keyof PerformanceMetricsResponse] as number | null | undefined
}

function hasMetricData(data: PerformanceMetricsResponse | undefined): boolean {
  if (!data) return false
  return METRICS.some((metric) => metricValue(data, metric.key) != null)
}

function renderSummary(data: PerformanceMetricsResponse): void {
  console.log(
    `  ${chalk.gray('Metric'.padEnd(10))}${chalk.gray('Average'.padStart(12))}${chalk.gray('p75'.padStart(12))}${chalk.gray('p90'.padStart(12))}${chalk.gray('p95'.padStart(12))}${chalk.gray('p99'.padStart(12))}`
  )
  console.log(`  ${chalk.gray('─'.repeat(70))}`)

  for (const metric of METRICS) {
    const average = formatMetric(metricValue(data, metric.key), metric.unit)
    const p75 = formatMetric(metricValue(data, metric.key, 'p75'), metric.unit)
    const p90 = formatMetric(metricValue(data, metric.key, 'p90'), metric.unit)
    const p95 = formatMetric(metricValue(data, metric.key, 'p95'), metric.unit)
    const p99 = formatMetric(metricValue(data, metric.key, 'p99'), metric.unit)
    console.log(
      `  ${chalk.white(metric.label.padEnd(10))}${average.padStart(12)}${p75.padStart(12)}${p90.padStart(12)}${p95.padStart(12)}${p99.padStart(12)}`
    )
  }
}

function formatGroupKey(value: string): string {
  const safeValue = sanitizeTerminalText(value)
  return safeValue.length > 34 ? `${safeValue.slice(0, 31)}...` : safeValue
}

function renderBreakdown(groups: GroupedPageMetric[], groupedBy: string, totalEvents: number): void {
  newline()
  console.log(`  ${chalk.bold.white(`Breakdown by ${sanitizeTerminalText(groupedBy)}`)}`)
  console.log(
    `  ${chalk.gray('Value'.padEnd(36))}${chalk.gray('Samples'.padStart(10))}${chalk.gray('LCP'.padStart(10))}${chalk.gray('INP'.padStart(10))}${chalk.gray('CLS'.padStart(10))}${chalk.gray('FCP'.padStart(10))}${chalk.gray('TTFB'.padStart(10))}`
  )
  console.log(`  ${chalk.gray('─'.repeat(96))}`)

  for (const group of groups) {
    console.log(
      `  ${chalk.white(formatGroupKey(group.group_key).padEnd(36))}${group.events.toLocaleString('en-US').padStart(10)}${formatMetric(group.lcp, 'ms').padStart(10)}${formatMetric(group.inp, 'ms').padStart(10)}${formatMetric(group.cls, 'score').padStart(10)}${formatMetric(group.fcp, 'ms').padStart(10)}${formatMetric(group.ttfb, 'ms').padStart(10)}`
    )
  }
  console.log(`  ${chalk.gray(`Total samples: ${totalEvents.toLocaleString('en-US')}`)}`)
}

export async function performanceInsights(options: PerformanceOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const window = resolvePerformanceWindow(options)
  const groupBy = parseGroupBy(options.groupBy)
  const resolved = await requireProjectSlug(options.project)
  const safeProjectSlug = sanitizeTerminalText(resolved.slug)

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(safeProjectSlug)} (from ${resolved.source})`)
  }

  const { data: project, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (projectError) {
    throw new Error(sanitizeTerminalText(getErrorMessage(projectError)))
  }
  if (!project) {
    throw new Error(`Project "${safeProjectSlug}" not found`)
  }

  const query = buildPerformanceQuery(project.id, window, options)
  const result = await withSpinner('Fetching Performance Insights...', () =>
    fetchPerformanceData(query, groupBy)
  )

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      period: window.period,
      start_date: window.startDate,
      end_date: window.endDate,
      filters: {
        environment_id: query.environment_id,
        deployment_id: query.deployment_id,
        device_type: query.device_type,
        include_bots: query.include_bots,
        path: query.filter_path,
        country: query.filter_country,
        region: query.filter_region,
        city: query.filter_city,
        browser: query.filter_browser,
        operating_system: query.filter_operating_system,
      },
      summary: result.summary,
      timeline: result.timeline,
      breakdown: result.breakdown,
    })
    return
  }

  const line = chalk.cyan('━'.repeat(72))
  const deviceLabel = query.device_type ? ` · ${query.device_type}` : ' · all devices'
  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('Performance Insights:')} ${chalk.bold.cyan(safeProjectSlug)} ${chalk.gray(`(${window.label}${deviceLabel})`)}`
  )
  console.log(line)
  newline()

  if (!hasMetricData(result.summary)) {
    console.log(`  ${chalk.yellow('No Web Vitals samples were recorded for these filters.')}`)
  } else {
    renderSummary(result.summary as PerformanceMetricsResponse)
  }

  const timeline = result.timeline as MetricsOverTimeResponse | undefined
  if (timeline?.timestamps.length) {
    newline()
    console.log(`  ${chalk.gray(`Timeline buckets: ${timeline.timestamps.length}`)}`)
  }

  if (result.breakdown) {
    renderBreakdown(
      result.breakdown.groups,
      result.breakdown.grouped_by,
      result.breakdown.total_events
    )
  }

  newline()
  console.log(line)
  newline()
}

export const parseDeviceForTest = parseDevice
export const parseGroupByForTest = parseGroupBy
export const parsePositiveIntegerForTest = parsePositiveInteger
export const formatMetricForTest = formatMetric
export const formatGroupKeyForTest = formatGroupKey
