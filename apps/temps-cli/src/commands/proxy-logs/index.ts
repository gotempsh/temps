import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getProxyLogs,
  getProxyLogById,
  getProxyLogByRequestId,
  getTimeBucketStats,
  getTodayStats,
} from '../../api/sdk.gen.js'
import type { ProxyLogResponse, TimeBucketStats } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptText } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, info, warning, keyValue, formatRelativeTime } from '../../ui/output.js'

interface ListOptions {
  json?: boolean
  limit?: string
  page?: string
  projectId?: string
  environmentId?: string
  method?: string
  statusCode?: string
  host?: string
  path?: string
  startDate?: string
  endDate?: string
  sortBy?: string
  sortOrder?: string
  isBot?: boolean
  hasError?: boolean
}

interface ShowOptions {
  id: string
  projectId?: string
  json?: boolean
}

interface ByRequestOptions {
  requestId?: string
  projectId?: string
  json?: boolean
}

interface StatsOptions {
  json?: boolean
}

interface TodayOptions {
  json?: boolean
}

export function registerProxyLogsCommands(program: Command): void {
  const proxyLogs = program
    .command('proxy-logs')
    .alias('plogs')
    .description('View proxy request logs and statistics')

  proxyLogs
    .command('list')
    .alias('ls')
    .description('List proxy logs')
    .option('--json', 'Output in JSON format')
    .option('--limit <n>', 'Items per page (default: 20, max: 100)')
    .option('--page <n>', 'Page number')
    .option('--project-id <id>', 'Filter by project ID')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--method <method>', 'Filter by HTTP method (GET, POST, etc.)')
    .option('--status-code <code>', 'Filter by HTTP status code')
    .option('--host <host>', 'Filter by host')
    .option('--path <path>', 'Filter by path (partial match)')
    .option('--start-date <date>', 'Start date (ISO 8601)')
    .option('--end-date <date>', 'End date (ISO 8601)')
    .option('--sort-by <field>', 'Sort by field (default: timestamp)')
    .option('--sort-order <order>', 'Sort order: asc or desc (default: desc)')
    .option('--is-bot', 'Filter for bot requests only')
    .option('--has-error', 'Filter for requests with errors only')
    .action(listProxyLogsAction)

  proxyLogs
    .command('show')
    .description('Show proxy log details')
    .requiredOption('--id <id>', 'Proxy log ID')
    .option('--project-id <id>', 'Authorize the lookup within this project')
    .option('--json', 'Output in JSON format')
    .action(showProxyLogAction)

  proxyLogs
    .command('by-request')
    .description('Get proxy log by request ID')
    .option('--request-id <id>', 'Request ID')
    .option('--project-id <id>', 'Authorize the lookup within this project')
    .option('--json', 'Output in JSON format')
    .action(byRequestAction)

  proxyLogs
    .command('stats')
    .description('Get time bucket statistics (last 24 hours)')
    .option('--json', 'Output in JSON format')
    .action(statsAction)

  proxyLogs
    .command('today')
    .description("Get today's request statistics")
    .option('--json', 'Output in JSON format')
    .action(todayAction)
}

/** Sums and averages a page of time-bucket stats into the printed summary line. */
export function summarizeBucketStats(
  stats: Array<{ request_count: number; error_count: number; avg_response_time_ms: number }>
): { totalRequests: number; totalErrors: number; avgResponseTime: number; errorRatePercent: number } {
  const totalRequests = stats.reduce((sum, s) => sum + s.request_count, 0)
  const totalErrors = stats.reduce((sum, s) => sum + s.error_count, 0)
  const avgResponseTime = stats.length > 0
    ? stats.reduce((sum, s) => sum + s.avg_response_time_ms, 0) / stats.length
    : 0
  const errorRatePercent = totalRequests > 0 ? (totalErrors / totalRequests) * 100 : 0
  return { totalRequests, totalErrors, avgResponseTime, errorRatePercent }
}

function statusCodeColor(code: number): string {
  if (code >= 200 && code < 300) return colors.success(code.toString())
  if (code >= 300 && code < 400) return colors.muted(code.toString())
  if (code >= 400 && code < 500) return colors.warning(code.toString())
  return colors.error(code.toString())
}

export function parseOptionalProjectId(value: string | undefined): number | undefined {
  if (value === undefined) return undefined
  if (!/^\d+$/.test(value)) throw new Error('Project ID must be a positive integer')
  const projectId = parseInt(value, 10)
  if (projectId < 1) throw new Error('Project ID must be a positive integer')
  return projectId
}

async function listProxyLogsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const pageSize = Math.min(parseInt(options.limit || '20', 10), 100)
  const page = options.page ? parseInt(options.page, 10) : 1
  const projectId = options.projectId ? parseInt(options.projectId, 10) : undefined
  const environmentId = options.environmentId ? parseInt(options.environmentId, 10) : undefined
  const statusCode = options.statusCode ? parseInt(options.statusCode, 10) : undefined

  const result = await withSpinner('Fetching proxy logs...', async () => {
    const { data, error } = await getProxyLogs({
      client,
      query: {
        page,
        page_size: pageSize,
        ...(projectId && { project_id: projectId }),
        ...(environmentId && { environment_id: environmentId }),
        ...(options.method && { method: options.method }),
        ...(statusCode && { status_code: statusCode }),
        ...(options.host && { host: options.host }),
        ...(options.path && { path: options.path }),
        ...(options.startDate && { start_date: options.startDate }),
        ...(options.endDate && { end_date: options.endDate }),
        ...(options.sortBy && { sort_by: options.sortBy }),
        ...(options.sortOrder && { sort_order: options.sortOrder }),
        ...(options.isBot !== undefined && { is_bot: options.isBot }),
        ...(options.hasError !== undefined && { has_error: options.hasError }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!result) {
    warning('No data returned')
    return
  }

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Proxy Logs (${result.logs.length} of ${result.total})`)

  if (result.logs.length === 0) {
    info('No proxy logs found')
    newline()
    return
  }

  const columns: TableColumn<ProxyLogResponse>[] = [
    { header: 'ID', key: 'id', width: 8 },
    { header: 'Method', key: 'method', color: (v) => colors.bold(v) },
    { header: 'Status', accessor: (l) => l.status_code.toString(), color: (v) => statusCodeColor(parseInt(v, 10)) },
    { header: 'Host', key: 'host', color: (v) => colors.muted(v.length > 30 ? v.slice(0, 30) + '...' : v) },
    { header: 'Path', key: 'path', color: (v) => colors.muted(v.length > 30 ? v.slice(0, 30) + '...' : v) },
    { header: 'Time', accessor: (l) => l.response_time_ms != null ? `${l.response_time_ms}ms` : '-' },
    { header: 'When', accessor: (l) => formatRelativeTime(l.timestamp) },
  ]

  printTable(result.logs, columns, { style: 'minimal' })

  if (result.total_pages > 1) {
    info(`Page ${result.page} of ${result.total_pages} (${result.total} total logs)`)
  }
  newline()
}

async function showProxyLogAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  const projectId = parseOptionalProjectId(options.projectId)
  if (isNaN(id)) {
    warning('Invalid proxy log ID')
    return
  }

  const log = await withSpinner('Fetching proxy log...', async () => {
    const { data, error } = await getProxyLogById({
      client,
      path: { id },
      query: { project_id: projectId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Proxy log ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(log)
    return
  }

  printProxyLogDetail(log)
}

async function byRequestAction(options: ByRequestOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let requestId: string
  const projectId = parseOptionalProjectId(options.projectId)

  if (options.requestId) {
    requestId = options.requestId
  } else {
    requestId = await promptText({
      message: 'Request ID',
      required: true,
    })
  }

  const log = await withSpinner(`Fetching log for request ${requestId}...`, async () => {
    const { data, error } = await getProxyLogByRequestId({
      client,
      path: { request_id: requestId },
      query: { project_id: projectId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Log for request ${requestId} not found`)
    }
    return data
  })

  if (options.json) {
    json(log)
    return
  }

  printProxyLogDetail(log)
}

async function statsAction(options: StatsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const endTime = new Date()
  const startTime = new Date()
  startTime.setHours(startTime.getHours() - 24)

  const result = await withSpinner('Fetching time bucket statistics...', async () => {
    const { data, error } = await getTimeBucketStats({
      client,
      query: {
        start_time: startTime.toISOString(),
        end_time: endTime.toISOString(),
        bucket_interval: '1 hour',
      },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to fetch statistics')
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Proxy Statistics (Last 24 Hours)`)
  keyValue('Interval', result.bucket_interval)
  keyValue('Start', new Date(result.start_time).toLocaleString())
  keyValue('End', new Date(result.end_time).toLocaleString())

  if (result.stats.length === 0) {
    newline()
    info('No statistics available for this period')
    newline()
    return
  }

  newline()

  const columns: TableColumn<TimeBucketStats>[] = [
    { header: 'Time', accessor: (s) => new Date(s.bucket).toLocaleTimeString() },
    { header: 'Requests', accessor: (s) => s.request_count.toString(), color: (v) => colors.bold(v) },
    { header: 'Errors', accessor: (s) => s.error_count.toString(), color: (v) => parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v) },
    { header: 'Avg Time', accessor: (s) => `${Math.round(s.avg_response_time_ms)}ms` },
    { header: 'Req Bytes', accessor: (s) => formatBytes(s.total_request_bytes), color: (v) => colors.muted(v) },
    { header: 'Res Bytes', accessor: (s) => formatBytes(s.total_response_bytes), color: (v) => colors.muted(v) },
  ]

  printTable(result.stats, columns, { style: 'minimal' })

  // Summary
  const { totalRequests, totalErrors, avgResponseTime, errorRatePercent } = summarizeBucketStats(result.stats)

  newline()
  header('Summary')
  keyValue('Total Requests', totalRequests.toLocaleString())
  keyValue('Total Errors', totalErrors > 0 ? colors.error(totalErrors.toLocaleString()) : '0')
  keyValue('Error Rate', totalRequests > 0 ? `${errorRatePercent.toFixed(2)}%` : '0%')
  keyValue('Avg Response Time', `${Math.round(avgResponseTime)}ms`)
  newline()
}

async function todayAction(options: TodayOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner("Fetching today's statistics...", async () => {
    const { data, error } = await getTodayStats({
      client,
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? "Failed to fetch today's statistics")
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Today's Statistics`)
  keyValue('Date', result.date)
  keyValue('Total Requests', colors.bold(result.total_requests.toLocaleString()))
  newline()
}

function printProxyLogDetail(log: ProxyLogResponse): void {
  newline()
  header(`${icons.info} Proxy Log #${log.id}`)

  // Request details
  keyValue('Request ID', log.request_id)
  keyValue('Method', colors.bold(log.method))
  keyValue('Host', log.host)
  keyValue('Path', log.path)
  if (log.query_string) {
    keyValue('Query String', log.query_string)
  }
  keyValue('Status Code', statusCodeColor(log.status_code))
  keyValue('Routing Status', log.routing_status)
  keyValue('Request Source', log.request_source)
  keyValue('System Request', log.is_system_request ? 'Yes' : 'No')
  keyValue('Timestamp', new Date(log.timestamp).toLocaleString())

  // Performance
  newline()
  header('Performance')
  keyValue('Response Time', log.response_time_ms != null ? `${log.response_time_ms}ms` : '-')
  keyValue('Request Size', log.request_size_bytes != null ? formatBytes(log.request_size_bytes) : '-')
  keyValue('Response Size', log.response_size_bytes != null ? formatBytes(log.response_size_bytes) : '-')
  if (log.cache_status) {
    keyValue('Cache Status', log.cache_status)
  }

  // Client info
  newline()
  header('Client')
  keyValue('Client IP', log.client_ip || '-')
  keyValue('User Agent', log.user_agent ? (log.user_agent.length > 80 ? log.user_agent.slice(0, 80) + '...' : log.user_agent) : '-')
  if (log.browser) keyValue('Browser', `${log.browser}${log.browser_version ? ` ${log.browser_version}` : ''}`)
  if (log.operating_system) keyValue('OS', log.operating_system)
  if (log.device_type) keyValue('Device', log.device_type)
  keyValue('Bot', log.is_bot ? colors.warning(`Yes${log.bot_name ? ` (${log.bot_name})` : ''}`) : 'No')
  if (log.referrer) keyValue('Referrer', log.referrer)

  // Routing
  if (log.project_id || log.environment_id || log.deployment_id || log.container_id || log.upstream_host) {
    newline()
    header('Routing')
    if (log.project_id != null) keyValue('Project ID', log.project_id)
    if (log.environment_id != null) keyValue('Environment ID', log.environment_id)
    if (log.deployment_id != null) keyValue('Deployment ID', log.deployment_id)
    if (log.container_id) keyValue('Container ID', log.container_id)
    if (log.upstream_host) keyValue('Upstream Host', log.upstream_host)
  }

  // Error
  if (log.error_message) {
    newline()
    header('Error')
    console.log(`  ${colors.error(log.error_message)}`)
  }

  newline()
}

export function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB']
  const i = Math.floor(Math.log(bytes) / Math.log(1024))
  const value = bytes / Math.pow(1024, i)
  return `${value.toFixed(i > 0 ? 1 : 0)} ${units[i]}`
}
