import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listIncidents,
  createIncident,
  getIncident,
  updateIncidentStatus,
  getIncidentUpdates,
  getBucketedIncidents,
} from '../../api/sdk.gen.js'
import type { IncidentResponse, IncidentUpdateResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptSelect } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, info, warning,
  keyValue, formatDate,
} from '../../ui/output.js'

const SEVERITY_LEVELS = [
  { name: 'Critical', value: 'critical' },
  { name: 'Major', value: 'major' },
  { name: 'Minor', value: 'minor' },
]

const INCIDENT_STATUSES = [
  { name: 'Investigating', value: 'investigating' },
  { name: 'Identified', value: 'identified' },
  { name: 'Monitoring', value: 'monitoring' },
  { name: 'Resolved', value: 'resolved' },
]

interface ListOptions {
  projectId: string
  status?: string
  environmentId?: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  title?: string
  description?: string
  severity?: string
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface UpdateStatusOptions {
  id: string
  status?: string
  message?: string
}

interface UpdatesOptions {
  id: string
  json?: boolean
}

interface BucketedOptions {
  projectId: string
  interval?: string
  startTime?: string
  endTime?: string
  environmentId?: string
  json?: boolean
}

export function registerIncidentsCommands(program: Command): void {
  const incidents = program
    .command('incidents')
    .alias('incident')
    .description('Manage incidents for status pages and monitoring')

  incidents
    .command('list')
    .alias('ls')
    .description('List incidents for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--status <status>', 'Filter by status (investigating, identified, monitoring, resolved)')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--page <n>', 'Page number')
    .option('--page-size <n>', 'Items per page')
    .option('--json', 'Output in JSON format')
    .action(listIncidentsAction)

  incidents
    .command('create')
    .alias('add')
    .description('Create a new incident')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-t, --title <title>', 'Incident title')
    .option('-d, --description <description>', 'Incident description')
    .option('-s, --severity <severity>', 'Severity level (critical, major, minor)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createIncidentAction)

  incidents
    .command('show')
    .description('Show incident details')
    .requiredOption('--id <id>', 'Incident ID')
    .option('--json', 'Output in JSON format')
    .action(showIncidentAction)

  incidents
    .command('update-status')
    .description('Update an incident status')
    .requiredOption('--id <id>', 'Incident ID')
    .option('-s, --status <status>', 'New status (investigating, identified, monitoring, resolved)')
    .option('-m, --message <message>', 'Status update message')
    .action(updateStatusAction)

  incidents
    .command('updates')
    .description('List status updates for an incident')
    .requiredOption('--id <id>', 'Incident ID')
    .option('--json', 'Output in JSON format')
    .action(listUpdatesAction)

  incidents
    .command('bucketed')
    .description('Get bucketed incident data for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-i, --interval <interval>', 'Bucket interval: 5min, hourly, daily (default: hourly)')
    .option('--start-time <time>', 'Start time (ISO 8601)')
    .option('--end-time <time>', 'End time (ISO 8601)')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--json', 'Output in JSON format')
    .action(bucketedAction)
}

export function severityColor(severity: string): string {
  switch (severity) {
    case 'critical':
      return colors.error(severity)
    case 'major':
      return colors.warning(severity)
    case 'minor':
      return colors.muted(severity)
    default:
      return severity
  }
}

export function incidentStatusBadge(status: string): string {
  switch (status) {
    case 'resolved':
      return statusBadge('active')
    case 'investigating':
      return colors.error(status)
    case 'identified':
      return colors.warning(status)
    case 'monitoring':
      return colors.primary(status)
    default:
      return status
  }
}

export function isValidSeverity(severity: string): boolean {
  return SEVERITY_LEVELS.some((s) => s.value === severity)
}

export function isValidIncidentStatus(status: string): boolean {
  return INCIDENT_STATUSES.some((s) => s.value === status)
}

/**
 * The list endpoint may return a bare array or a paginated `{ data: [...] }`
 * envelope. Typed `unknown` because the SDK's generated response type for
 * this endpoint is `unknown` (no response body schema in the OpenAPI spec).
 */
export function extractIncidentsList(data: unknown): IncidentResponse[] {
  if (Array.isArray(data)) return data as IncidentResponse[]
  return (data as { data?: IncidentResponse[] } | undefined)?.data ?? []
}

export function buildBucketedQuery(options: {
  interval?: string
  startTime?: string
  endTime?: string
  environmentId?: string
}): Record<string, string | number> {
  const query: Record<string, string | number> = {}
  if (options.interval) {
    query.interval = options.interval
  }
  if (options.startTime) {
    query.start_time = options.startTime
  }
  if (options.endTime) {
    query.end_time = options.endTime
  }
  if (options.environmentId) {
    const envId = parseInt(options.environmentId, 10)
    if (!isNaN(envId)) {
      query.environment_id = envId
    }
  }
  return query
}

async function listIncidentsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const incidentsData = await withSpinner('Fetching incidents...', async () => {
    const page = options.page ? parseInt(options.page, 10) : undefined
    const pageSize = options.pageSize ? parseInt(options.pageSize, 10) : undefined
    const environmentId = options.environmentId ? parseInt(options.environmentId, 10) : undefined

    const { data, error } = await listIncidents({
      client,
      path: { project_id: projectId },
      query: {
        ...(options.status && { status: options.status }),
        ...(environmentId && { environment_id: environmentId }),
        ...(page && { page }),
        ...(pageSize && { page_size: pageSize }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    // data may be an array or a paginated response
    return extractIncidentsList(data)
  })

  if (options.json) {
    json(incidentsData)
    return
  }

  newline()
  header(`${icons.warning} Incidents for Project ${projectId} (${incidentsData.length})`)

  if (incidentsData.length === 0) {
    info('No incidents found')
    info(`Run: temps incidents create --project-id ${projectId} --title "Service degradation" --severity major -y`)
    newline()
    return
  }

  const columns: TableColumn<IncidentResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Title', key: 'title', color: (v) => colors.bold(v) },
    { header: 'Severity', key: 'severity', color: (v) => severityColor(v) },
    { header: 'Status', key: 'status', color: (v) => incidentStatusBadge(v) },
    { header: 'Started', accessor: (i) => formatDate(i.started_at), color: (v) => colors.muted(v) },
    { header: 'Resolved', accessor: (i) => i.resolved_at ? formatDate(i.resolved_at) : '-', color: (v) => colors.muted(v) },
  ]

  printTable(incidentsData, columns, { style: 'minimal' })
  newline()
}

async function createIncidentAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let title: string
  let description: string | null = null
  let severity: string

  const isAutomation = options.yes && options.title && options.severity

  if (isAutomation) {
    title = options.title!
    description = options.description || null
    severity = options.severity!

    if (!isValidSeverity(severity)) {
      const validSeverities = SEVERITY_LEVELS.map(s => s.value)
      warning(`Invalid severity: ${severity}. Available: ${validSeverities.join(', ')}`)
      return
    }
  } else {
    // Interactive mode
    title = options.title || await promptText({
      message: 'Incident title',
      required: true,
    })

    description = options.description || await promptText({
      message: 'Description (optional)',
      default: '',
    }) || null

    severity = options.severity || await promptSelect({
      message: 'Severity level',
      choices: SEVERITY_LEVELS,
    })
  }

  const incident = await withSpinner('Creating incident...', async () => {
    const { data, error } = await createIncident({
      client,
      path: { project_id: projectId },
      body: {
        title,
        severity,
        ...(description && { description }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  newline()
  success(`Incident "${title}" created`)
  if (incident) {
    keyValue('ID', incident.id)
    keyValue('Status', incidentStatusBadge(incident.status))
    keyValue('Severity', severityColor(incident.severity))
  }
  newline()
  info('Run: temps incidents update-status --id <id> --status identified --message "Root cause found"')
}

async function showIncidentAction(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid incident ID')
    return
  }

  const incident = await withSpinner('Fetching incident...', async () => {
    const { data, error } = await getIncident({
      client,
      path: { incident_id: id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Incident ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(incident)
    return
  }

  newline()
  header(`${icons.warning} ${incident.title}`)
  keyValue('ID', incident.id)
  keyValue('Project ID', incident.project_id)
  keyValue('Status', incidentStatusBadge(incident.status))
  keyValue('Severity', severityColor(incident.severity))
  if (incident.description) {
    keyValue('Description', incident.description)
  }
  if (incident.monitor_id) {
    keyValue('Monitor ID', incident.monitor_id)
  }
  if (incident.environment_id) {
    keyValue('Environment ID', incident.environment_id)
  }
  keyValue('Started', formatDate(incident.started_at))
  if (incident.resolved_at) {
    keyValue('Resolved', formatDate(incident.resolved_at))
  }
  keyValue('Created', formatDate(incident.created_at))
  keyValue('Updated', formatDate(incident.updated_at))
  newline()
}

async function updateStatusAction(options: UpdateStatusOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid incident ID')
    return
  }

  let status: string
  let message: string

  if (options.status && options.message) {
    status = options.status
    message = options.message
  } else {
    // Interactive mode
    status = options.status || await promptSelect({
      message: 'New incident status',
      choices: INCIDENT_STATUSES,
    })

    message = options.message || await promptText({
      message: 'Status update message',
      required: true,
    })
  }

  if (!isValidIncidentStatus(status)) {
    const validStatuses = INCIDENT_STATUSES.map(s => s.value)
    warning(`Invalid status: ${status}. Available: ${validStatuses.join(', ')}`)
    return
  }

  const incident = await withSpinner('Updating incident status...', async () => {
    const { data, error } = await updateIncidentStatus({
      client,
      path: { incident_id: id },
      body: {
        status,
        message,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  newline()
  success(`Incident status updated to ${incidentStatusBadge(status)}`)
  if (incident) {
    keyValue('ID', incident.id)
    keyValue('Title', incident.title)
    keyValue('Status', incidentStatusBadge(incident.status))
  }
  newline()
}

async function listUpdatesAction(options: UpdatesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid incident ID')
    return
  }

  const updates = await withSpinner('Fetching incident updates...', async () => {
    const { data, error } = await getIncidentUpdates({
      client,
      path: { incident_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(updates)
    return
  }

  newline()
  header(`${icons.info} Incident Updates (${updates.length})`)

  if (updates.length === 0) {
    info('No status updates for this incident')
    newline()
    return
  }

  const columns: TableColumn<IncidentUpdateResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Status', key: 'status', color: (v) => incidentStatusBadge(v) },
    { header: 'Message', key: 'message' },
    { header: 'Created', accessor: (u) => formatDate(u.created_at), color: (v) => colors.muted(v) },
  ]

  printTable(updates, columns, { style: 'minimal' })
  newline()
}

async function bucketedAction(options: BucketedOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const query = buildBucketedQuery(options)

  const result = await withSpinner('Fetching bucketed incident data...', async () => {
    const { data, error } = await getBucketedIncidents({
      client,
      path: { project_id: projectId },
      query,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} Bucketed Incidents for Project ${projectId}`)

  if (!result || !result.buckets || result.buckets.length === 0) {
    info('No bucketed data available')
    newline()
    return
  }

  keyValue('Interval', result.interval)
  keyValue('Project ID', result.project_id)
  if (result.environment_id) {
    keyValue('Environment ID', result.environment_id)
  }
  newline()

  for (const bucket of result.buckets) {
    const date = formatDate(bucket.bucket_start)
    const total = bucket.total_incidents
    const active = bucket.active_incidents
    const resolved = bucket.resolved_incidents

    const criticalStr = bucket.critical_incidents > 0 ? colors.error(`${bucket.critical_incidents} critical`) : ''
    const majorStr = bucket.major_incidents > 0 ? colors.warning(`${bucket.major_incidents} major`) : ''
    const minorStr = bucket.minor_incidents > 0 ? colors.muted(`${bucket.minor_incidents} minor`) : ''
    const parts = [criticalStr, majorStr, minorStr].filter(Boolean).join(', ')

    const resolvedStr = resolved > 0 ? colors.success(` | ${resolved} resolved`) : ''
    const avgStr = bucket.avg_resolution_time_minutes ? colors.muted(` | avg ${Math.round(bucket.avg_resolution_time_minutes)}min`) : ''

    console.log(`  ${colors.bold(date)}: ${total} total (${active} active${resolvedStr}${avgStr})`)
    if (parts) {
      console.log(`    ${parts}`)
    }
  }
  newline()
}
