import type { Command } from 'commander'
import { requireAuth, config, credentials } from '../../config/store.js'
import { setupClient, client, getErrorMessage, normalizeApiUrl } from '../../lib/api-client.js'
import {
  listErrorGroups,
  getErrorGroup,
  updateErrorGroup,
  listErrorEvents,
  getErrorEvent,
  getErrorStats,
  getErrorTimeSeries,
  getErrorDashboardStats,
} from '../../api/sdk.gen.js'
import type { ErrorGroupResponse, ErrorEventResponse, ErrorTimeSeriesDataResponse } from '../../api/types.gen.js'
import { withSpinner, startSpinner, succeedSpinner, failSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, formatRelativeTime } from '../../ui/output.js'
import { readFile } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { resolve, basename } from 'node:path'

interface ListOptions {
  projectId: string
  status?: string
  page?: string
  pageSize?: string
  environmentId?: string
  startDate?: string
  endDate?: string
  sortBy?: string
  sortOrder?: string
  json?: boolean
}

interface ShowOptions {
  projectId: string
  groupId: string
  json?: boolean
}

interface UpdateOptions {
  projectId: string
  groupId: string
  status: string
  assignedTo?: string
}

interface EventsListOptions {
  projectId: string
  groupId: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface EventShowOptions {
  projectId: string
  groupId: string
  eventId: string
  json?: boolean
}

interface StatsOptions {
  projectId: string
  json?: boolean
}

interface TimelineOptions {
  projectId: string
  days?: string
  bucket?: string
  json?: boolean
}

interface DashboardOptions {
  projectId: string
  days?: string
  compare?: boolean
  json?: boolean
}

const ERROR_STATUSES = ['unresolved', 'resolved', 'ignored']

// Source map interfaces
interface SourceMapUploadOptions {
  projectId: string
  release: string
  file: string
  filePath?: string
  dist?: string
}

interface SourceMapListOptions {
  projectId: string
  release: string
  json?: boolean
}

interface SourceMapReleasesOptions {
  projectId: string
  json?: boolean
}

interface SourceMapDeleteReleaseOptions {
  projectId: string
  release: string
}

interface SourceMapDeleteOneOptions {
  projectId: string
  sourceMapId: string
}

// Source map API response shapes
interface SourceMapResponse {
  id: number
  project_id: number
  release: string
  file_path: string
  dist?: string
  size_bytes: number
  checksum?: string
  created_at: string
}

interface SourceMapListResponse {
  source_maps: SourceMapResponse[]
  total: number
}

interface ReleaseListResponse {
  releases: string[]
}

interface DeleteResponse {
  deleted: number
}

export function registerErrorsCommands(program: Command): void {
  const errors = program
    .command('errors')
    .alias('error')
    .description('Manage error tracking and error groups')

  errors
    .command('list')
    .alias('ls')
    .description('List error groups for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--status <status>', 'Filter by status (unresolved, resolved, ignored)')
    .option('--page <page>', 'Page number')
    .option('--page-size <size>', 'Page size')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--start-date <date>', 'Filter by start date (ISO 8601)')
    .option('--end-date <date>', 'Filter by end date (ISO 8601)')
    .option('--sort-by <field>', 'Sort by field (e.g., total_count, last_seen, first_seen)')
    .option('--sort-order <order>', 'Sort order: asc or desc')
    .option('--json', 'Output in JSON format')
    .action(listErrorGroupsAction)

  errors
    .command('show')
    .description('Show error group details')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--json', 'Output in JSON format')
    .action(showErrorGroup)

  errors
    .command('update')
    .description('Update error group status')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .requiredOption('--status <status>', 'New status (unresolved, resolved, ignored)')
    .option('--assigned-to <user>', 'Assign to user')
    .action(updateErrorGroupAction)

  errors
    .command('events')
    .description('List events in an error group')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--page <page>', 'Page number')
    .option('--page-size <size>', 'Page size')
    .option('--json', 'Output in JSON format')
    .action(listErrorEventsAction)

  errors
    .command('event')
    .description('Show a specific error event')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .requiredOption('--event-id <id>', 'Error event ID')
    .option('--json', 'Output in JSON format')
    .action(showErrorEvent)

  errors
    .command('stats')
    .description('Get error statistics for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(getErrorStatsAction)

  errors
    .command('timeline')
    .description('Get error time series data')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--days <days>', 'Number of days to show', '7')
    .option('--bucket <bucket>', 'Time bucket size (e.g., "1h", "15m", "1d")', '1h')
    .option('--json', 'Output in JSON format')
    .action(getErrorTimelineAction)

  errors
    .command('dashboard')
    .description('Get error dashboard statistics')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--days <days>', 'Number of days to show', '7')
    .option('--compare', 'Compare to previous period')
    .option('--json', 'Output in JSON format')
    .action(getErrorDashboardAction)

  // Source maps subcommand group
  const sourcemaps = errors
    .command('sourcemaps')
    .alias('sm')
    .description('Manage source maps for error symbolication')

  sourcemaps
    .command('upload')
    .description('Upload a source map file for a release')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--release <version>', 'Release version (e.g. commit SHA)')
    .requiredOption('--file <path>', 'Path to the .map file')
    .option('--file-path <urlpath>', 'URL path in stack traces (e.g. ~/assets/main.js)')
    .option('--dist <dist>', 'Distribution identifier')
    .action(uploadSourceMap)

  sourcemaps
    .command('list')
    .alias('ls')
    .description('List source maps for a release')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--release <version>', 'Release version')
    .option('--json', 'Output in JSON format')
    .action(listSourceMaps)

  sourcemaps
    .command('releases')
    .description('List all releases that have source maps')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listSourceMapReleases)

  sourcemaps
    .command('delete')
    .description('Delete all source maps for a release')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--release <version>', 'Release version')
    .action(deleteReleaseSourceMaps)

  sourcemaps
    .command('delete-one')
    .description('Delete a specific source map by ID')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--source-map-id <id>', 'Source map ID')
    .action(deleteSourceMap)
}

async function listErrorGroupsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  if (options.status && !ERROR_STATUSES.includes(options.status)) {
    warning(`Invalid status: ${options.status}. Available: ${ERROR_STATUSES.join(', ')}`)
    return
  }

  const environmentId = options.environmentId ? parseInt(options.environmentId, 10) : undefined

  const result = await withSpinner('Fetching error groups...', async () => {
    const { data, error } = await listErrorGroups({
      client,
      path: { project_id: projectId },
      query: {
        page: options.page ? parseInt(options.page, 10) : undefined,
        page_size: options.pageSize ? parseInt(options.pageSize, 10) : undefined,
        status: options.status ?? undefined,
        ...(environmentId && { environment_id: environmentId }),
        ...(options.startDate && { start_date: options.startDate }),
        ...(options.endDate && { end_date: options.endDate }),
        ...(options.sortBy && { sort_by: options.sortBy }),
        ...(options.sortOrder && { sort_order: options.sortOrder }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const groups = result?.data ?? []
  const pagination = result?.pagination

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Error Groups for Project ${projectId} (${pagination?.total_count ?? groups.length})`)

  if (groups.length === 0) {
    info('No error groups found')
    newline()
    return
  }

  const columns: TableColumn<ErrorGroupResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Title', key: 'title', color: (v) => colors.bold(v.length > 50 ? v.slice(0, 50) + '...' : v) },
    { header: 'Type', key: 'error_type' },
    { header: 'Count', accessor: (g) => g.total_count.toString(), color: (v) => parseInt(v, 10) > 100 ? colors.error(v) : v },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v === 'resolved' ? 'success' : v === 'ignored' ? 'inactive' : 'error') },
    { header: 'Last Seen', accessor: (g) => formatRelativeTime(g.last_seen) },
  ]

  printTable(groups, columns, { style: 'minimal' })

  if (pagination && pagination.total_pages > 1) {
    info(`Page ${pagination.page} of ${pagination.total_pages} (${pagination.total_count} total)`)
  }
  newline()
}

async function showErrorGroup(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const groupId = parseInt(options.groupId, 10)
  if (isNaN(projectId) || isNaN(groupId)) {
    warning('Invalid project or group ID')
    return
  }

  const group = await withSpinner('Fetching error group...', async () => {
    const { data, error } = await getErrorGroup({
      client,
      path: { project_id: projectId, group_id: groupId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Error group ${options.groupId} not found`)
    }
    return data
  })

  if (options.json) {
    json(group)
    return
  }

  newline()
  header(`${icons.info} Error Group #${group.id}`)
  keyValue('Title', group.title)
  keyValue('Type', group.error_type)
  keyValue('Status', statusBadge(group.status === 'resolved' ? 'success' : group.status === 'ignored' ? 'inactive' : 'error'))
  keyValue('Total Count', group.total_count.toString())
  keyValue('Project ID', group.project_id.toString())

  if (group.message_template) {
    keyValue('Message', group.message_template)
  }
  if (group.assigned_to) {
    keyValue('Assigned To', group.assigned_to)
  }
  if (group.environment_id !== null && group.environment_id !== undefined) {
    keyValue('Environment ID', group.environment_id.toString())
  }
  if (group.deployment_id !== null && group.deployment_id !== undefined) {
    keyValue('Deployment ID', group.deployment_id.toString())
  }

  keyValue('First Seen', formatRelativeTime(group.first_seen))
  keyValue('Last Seen', formatRelativeTime(group.last_seen))
  keyValue('Created', new Date(group.created_at).toLocaleString())
  keyValue('Updated', new Date(group.updated_at).toLocaleString())
  newline()
}

async function updateErrorGroupAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const groupId = parseInt(options.groupId, 10)
  if (isNaN(projectId) || isNaN(groupId)) {
    warning('Invalid project or group ID')
    return
  }

  if (!ERROR_STATUSES.includes(options.status)) {
    warning(`Invalid status: ${options.status}. Available: ${ERROR_STATUSES.join(', ')}`)
    return
  }

  await withSpinner('Updating error group...', async () => {
    const { error } = await updateErrorGroup({
      client,
      path: { project_id: projectId, group_id: groupId },
      body: {
        status: options.status,
        assigned_to: options.assignedTo ?? undefined,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Error group #${groupId} updated`)
  info(`Status: ${options.status}`)
  if (options.assignedTo) {
    info(`Assigned to: ${options.assignedTo}`)
  }
}

async function listErrorEventsAction(options: EventsListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const groupId = parseInt(options.groupId, 10)
  if (isNaN(projectId) || isNaN(groupId)) {
    warning('Invalid project or group ID')
    return
  }

  const result = await withSpinner('Fetching error events...', async () => {
    const { data, error } = await listErrorEvents({
      client,
      path: { project_id: projectId, group_id: groupId },
      query: {
        page: options.page ? parseInt(options.page, 10) : undefined,
        page_size: options.pageSize ? parseInt(options.pageSize, 10) : undefined,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const events = result?.data ?? []
  const pagination = result?.pagination

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Error Events for Group #${groupId} (${pagination?.total_count ?? events.length})`)

  if (events.length === 0) {
    info('No error events found')
    newline()
    return
  }

  const columns: TableColumn<ErrorEventResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Source', accessor: (e) => e.source ?? '-' },
    { header: 'Timestamp', accessor: (e) => formatRelativeTime(e.timestamp) },
    { header: 'Created', accessor: (e) => formatRelativeTime(e.created_at) },
  ]

  printTable(events, columns, { style: 'minimal' })

  if (pagination && pagination.total_pages > 1) {
    info(`Page ${pagination.page} of ${pagination.total_pages} (${pagination.total_count} total)`)
  }
  newline()
}

async function showErrorEvent(options: EventShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const groupId = parseInt(options.groupId, 10)
  const eventId = parseInt(options.eventId, 10)
  if (isNaN(projectId) || isNaN(groupId) || isNaN(eventId)) {
    warning('Invalid project, group, or event ID')
    return
  }

  const event = await withSpinner('Fetching error event...', async () => {
    const { data, error } = await getErrorEvent({
      client,
      path: { project_id: projectId, group_id: groupId, event_id: eventId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Error event ${options.eventId} not found`)
    }
    return data
  })

  if (options.json) {
    json(event)
    return
  }

  newline()
  header(`${icons.info} Error Event #${event.id}`)
  keyValue('Group ID', event.error_group_id.toString())
  keyValue('Source', event.source ?? '-')
  keyValue('Timestamp', new Date(event.timestamp).toLocaleString())
  keyValue('Created', new Date(event.created_at).toLocaleString())

  if (event.data) {
    newline()
    header('Event Data')
    try {
      const dataStr = typeof event.data === 'string' ? event.data : JSON.stringify(event.data, null, 2)
      console.log(dataStr)
    } catch {
      console.log(String(event.data))
    }
  }
  newline()
}

async function getErrorStatsAction(options: StatsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const stats = await withSpinner('Fetching error statistics...', async () => {
    const { data, error } = await getErrorStats({
      client,
      path: { project_id: projectId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to get error statistics')
    }
    return data
  })

  if (options.json) {
    json(stats)
    return
  }

  newline()
  header(`${icons.info} Error Statistics for Project ${projectId}`)
  keyValue('Total Groups', stats.total_groups.toString())
  keyValue('Unresolved', colors.error(stats.unresolved_groups.toString()))
  keyValue('Resolved', colors.success(stats.resolved_groups.toString()))
  keyValue('Ignored', colors.muted(stats.ignored_groups.toString()))
  newline()
}

async function getErrorTimelineAction(options: TimelineOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const days = parseInt(options.days || '7', 10)
  const endTime = new Date()
  const startTime = new Date()
  startTime.setDate(startTime.getDate() - days)

  const timeSeries = await withSpinner('Fetching error timeline...', async () => {
    const { data, error } = await getErrorTimeSeries({
      client,
      path: { project_id: projectId },
      query: {
        start_time: startTime.toISOString(),
        end_time: endTime.toISOString(),
        bucket: options.bucket,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(timeSeries)
    return
  }

  newline()
  header(`${icons.info} Error Timeline (Last ${days} days, bucket: ${options.bucket || '1h'})`)

  if (timeSeries.length === 0) {
    info('No error data available for this period')
    newline()
    return
  }

  const columns: TableColumn<ErrorTimeSeriesDataResponse>[] = [
    { header: 'Timestamp', accessor: (e) => new Date(e.timestamp).toLocaleString() },
    { header: 'Errors', accessor: (e) => e.count.toString(), color: (v) => parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v) },
  ]

  printTable(timeSeries, columns, { style: 'minimal' })
  newline()
}

async function uploadSourceMap(options: SourceMapUploadOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const resolvedPath = resolve(options.file)
  if (!existsSync(resolvedPath)) {
    warning(`File does not exist: ${resolvedPath}`)
    return
  }

  const filename = basename(resolvedPath)
  startSpinner(`Uploading source map: ${filename}...`)

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const uploadUrl = `${apiUrl}/projects/${projectId}/releases/${encodeURIComponent(options.release)}/source-maps`

  try {
    const fileData = await readFile(resolvedPath)
    const uint8Data = new Uint8Array(fileData.buffer, fileData.byteOffset, fileData.byteLength)

    const formData = new FormData()
    formData.append('file', new Blob([uint8Data], { type: 'application/json' }), filename)
    if (options.filePath) {
      formData.append('file_path', options.filePath)
    }
    if (options.dist) {
      formData.append('dist', options.dist)
    }

    const response = await fetch(uploadUrl, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${apiKey}`,
      },
      body: formData,
    })

    if (!response.ok) {
      const errorText = await response.text()
      failSpinner(`Upload failed: ${response.status}`)
      warning(errorText)
      return
    }

    const result = (await response.json()) as SourceMapResponse
    succeedSpinner(`Source map uploaded (id=${result.id})`)

    newline()
    keyValue('ID', result.id.toString())
    keyValue('Release', result.release)
    keyValue('File Path', result.file_path)
    keyValue('Size', formatBytes(result.size_bytes))
    if (result.dist) keyValue('Dist', result.dist)
    newline()
  } catch (err) {
    failSpinner('Upload failed')
    throw err
  }
}

async function listSourceMaps(options: SourceMapListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const url = `${apiUrl}/projects/${projectId}/releases/${encodeURIComponent(options.release)}/source-maps`

  const result = await withSpinner('Fetching source maps...', async () => {
    const response = await fetch(url, {
      headers: { Authorization: `Bearer ${apiKey}` },
    })
    if (!response.ok) {
      const errorText = await response.text()
      throw new Error(`Request failed (${response.status}): ${errorText}`)
    }
    return response.json() as Promise<SourceMapListResponse>
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Source Maps for Release "${options.release}" (${result.total})`)

  if (result.source_maps.length === 0) {
    info('No source maps found for this release')
    newline()
    return
  }

  const columns: TableColumn<SourceMapResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'File Path', key: 'file_path' },
    { header: 'Dist', accessor: (m) => m.dist ?? '-' },
    { header: 'Size', accessor: (m) => formatBytes(m.size_bytes) },
    { header: 'Uploaded', accessor: (m) => formatRelativeTime(m.created_at) },
  ]

  printTable(result.source_maps, columns, { style: 'minimal' })
  newline()
}

async function listSourceMapReleases(options: SourceMapReleasesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const url = `${apiUrl}/projects/${projectId}/source-map-releases`

  const result = await withSpinner('Fetching releases...', async () => {
    const response = await fetch(url, {
      headers: { Authorization: `Bearer ${apiKey}` },
    })
    if (!response.ok) {
      const errorText = await response.text()
      throw new Error(`Request failed (${response.status}): ${errorText}`)
    }
    return response.json() as Promise<ReleaseListResponse>
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Releases with Source Maps for Project ${projectId}`)

  if (result.releases.length === 0) {
    info('No releases with source maps found')
    newline()
    return
  }

  for (const release of result.releases) {
    info(`  ${colors.bold(release)}`)
  }
  newline()
}

async function deleteReleaseSourceMaps(options: SourceMapDeleteReleaseOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const url = `${apiUrl}/projects/${projectId}/releases/${encodeURIComponent(options.release)}/source-maps`

  const result = await withSpinner(`Deleting source maps for release "${options.release}"...`, async () => {
    const response = await fetch(url, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${apiKey}` },
    })
    if (!response.ok) {
      const errorText = await response.text()
      throw new Error(`Request failed (${response.status}): ${errorText}`)
    }
    return response.json() as Promise<DeleteResponse>
  })

  success(`Deleted ${result.deleted} source map(s) for release "${options.release}"`)
}

async function deleteSourceMap(options: SourceMapDeleteOneOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  const sourceMapId = parseInt(options.sourceMapId, 10)
  if (isNaN(projectId) || isNaN(sourceMapId)) {
    warning('Invalid project ID or source map ID')
    return
  }

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const url = `${apiUrl}/projects/${projectId}/source-maps/${sourceMapId}`

  await withSpinner(`Deleting source map #${sourceMapId}...`, async () => {
    const response = await fetch(url, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${apiKey}` },
    })
    if (!response.ok && response.status !== 204) {
      const errorText = await response.text()
      throw new Error(`Request failed (${response.status}): ${errorText}`)
    }
  })

  success(`Source map #${sourceMapId} deleted`)
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`
}

async function getErrorDashboardAction(options: DashboardOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const days = parseInt(options.days || '7', 10)
  const endTime = new Date()
  const startTime = new Date()
  startTime.setDate(startTime.getDate() - days)

  const dashboard = await withSpinner('Fetching dashboard statistics...', async () => {
    const { data, error } = await getErrorDashboardStats({
      client,
      path: { project_id: projectId },
      query: {
        start_time: startTime.toISOString(),
        end_time: endTime.toISOString(),
        compare_to_previous: options.compare ?? null,
      },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to get dashboard statistics')
    }
    return data
  })

  if (options.json) {
    json(dashboard)
    return
  }

  newline()
  header(`${icons.info} Error Dashboard for Project ${projectId} (Last ${days} days)`)
  keyValue('Period', `${new Date(dashboard.start_time).toLocaleDateString()} - ${new Date(dashboard.end_time).toLocaleDateString()}`)
  keyValue('Total Errors', dashboard.total_errors.toString())
  keyValue('Error Groups', dashboard.error_groups.toString())

  if (options.compare) {
    newline()
    header('Comparison to Previous Period')
    keyValue('Previous Period Errors', dashboard.total_errors_previous_period.toString())
    keyValue('Previous Period Groups', dashboard.error_groups_previous_period.toString())

    const changePercent = dashboard.total_errors_change_percent
    const changeStr = changePercent >= 0 ? `+${changePercent.toFixed(1)}%` : `${changePercent.toFixed(1)}%`
    const changeColor = changePercent > 0 ? colors.error : changePercent < 0 ? colors.success : colors.muted
    keyValue('Change', changeColor(changeStr))

    if (dashboard.comparison_start_time && dashboard.comparison_end_time) {
      keyValue('Comparison Period', `${new Date(dashboard.comparison_start_time).toLocaleDateString()} - ${new Date(dashboard.comparison_end_time).toLocaleDateString()}`)
    }
  }
  newline()
}
