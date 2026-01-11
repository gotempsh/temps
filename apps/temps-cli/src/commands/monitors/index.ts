import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listMonitors,
  createMonitor,
  getMonitor,
  deleteMonitor,
  getCurrentMonitorStatus,
  getUptimeHistory,
} from '../../api/sdk.gen.js'
import type { MonitorResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

const MONITOR_TYPES = ['http', 'tcp', 'ping']
const CHECK_INTERVALS = [
  { name: '1 minute', value: 60 },
  { name: '5 minutes', value: 300 },
  { name: '10 minutes', value: 600 },
  { name: '15 minutes', value: 900 },
  { name: '30 minutes', value: 1800 },
]

interface ListOptions {
  projectId: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  name?: string
  type?: string
  interval?: string
  environmentId?: string
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface StatusOptions {
  id: string
  json?: boolean
}

interface HistoryOptions {
  id: string
  json?: boolean
  days?: string
}

export function registerMonitorsCommands(program: Command): void {
  const monitors = program
    .command('monitors')
    .description('Manage uptime monitors for status pages')

  monitors
    .command('list')
    .alias('ls')
    .description('List all monitors for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listMonitorsAction)

  monitors
    .command('create')
    .alias('add')
    .description('Create a new monitor for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-n, --name <name>', 'Monitor name')
    .option('-t, --type <type>', 'Monitor type (http, tcp, ping)')
    .option('-i, --interval <seconds>', 'Check interval in seconds (60, 300, 600, 900, 1800)')
    .option('--environment-id <id>', 'Environment ID (default: 0 for production)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createMonitorAction)

  monitors
    .command('show')
    .description('Show monitor details and current status')
    .requiredOption('--id <id>', 'Monitor ID')
    .option('--json', 'Output in JSON format')
    .action(showMonitor)

  monitors
    .command('remove')
    .alias('rm')
    .description('Delete a monitor')
    .requiredOption('--id <id>', 'Monitor ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeMonitor)

  monitors
    .command('status')
    .description('Get current monitor status')
    .requiredOption('--id <id>', 'Monitor ID')
    .option('--json', 'Output in JSON format')
    .action(getMonitorStatus)

  monitors
    .command('history')
    .description('Get monitor uptime history')
    .requiredOption('--id <id>', 'Monitor ID')
    .option('--json', 'Output in JSON format')
    .option('--days <days>', 'Number of days to show', '7')
    .action(getMonitorHistory)
}

async function listMonitorsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.projectId, 10)
  if (isNaN(id)) {
    warning('Invalid project ID')
    return
  }

  const monitorsData = await withSpinner('Fetching monitors...', async () => {
    const { data, error } = await listMonitors({
      client,
      path: { project_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(monitorsData)
    return
  }

  newline()
  header(`${icons.info} Monitors for Project ${id} (${monitorsData.length})`)

  if (monitorsData.length === 0) {
    info('No monitors configured')
    info(`Run: temps monitors create --project-id ${id} --name my-monitor --type http --interval 300 -y`)
    newline()
    return
  }

  const columns: TableColumn<MonitorResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'monitor_type' },
    { header: 'URL', key: 'monitor_url' },
    { header: 'Interval', accessor: (m) => `${m.check_interval_seconds}s` },
    { header: 'Status', accessor: (m) => m.is_active ? 'active' : 'paused', color: (v) => statusBadge(v === 'active' ? 'active' : 'inactive') },
  ]

  printTable(monitorsData, columns, { style: 'minimal' })
  newline()
}

async function createMonitorAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let name: string
  let monitorType: string
  let checkInterval: number
  let environmentId: number

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.name && options.type && options.interval

  if (isAutomation) {
    name = options.name!
    monitorType = options.type!
    checkInterval = parseInt(options.interval!, 10)
    environmentId = options.environmentId ? parseInt(options.environmentId, 10) : 0

    // Validate monitor type
    if (!MONITOR_TYPES.includes(monitorType)) {
      warning(`Invalid monitor type: ${monitorType}. Available: ${MONITOR_TYPES.join(', ')}`)
      return
    }

    // Validate check interval
    const validIntervals = CHECK_INTERVALS.map(c => c.value)
    if (!validIntervals.includes(checkInterval)) {
      warning(`Invalid interval: ${checkInterval}. Available: ${validIntervals.join(', ')}`)
      return
    }
  } else {
    // Interactive mode
    name = options.name || await promptText({
      message: 'Monitor name',
      required: true,
    })

    monitorType = options.type || await promptSelect({
      message: 'Monitor type',
      choices: MONITOR_TYPES.map(t => ({
        name: t.toUpperCase(),
        value: t,
      })),
    })

    checkInterval = options.interval
      ? parseInt(options.interval, 10)
      : await promptSelect({
          message: 'Check interval',
          choices: CHECK_INTERVALS,
        }) as number

    // For environment_id, we'd need to list environments - using 0 as default (production)
    environmentId = options.environmentId ? parseInt(options.environmentId, 10) : 0
  }

  await withSpinner('Creating monitor...', async () => {
    const { error } = await createMonitor({
      client,
      path: { project_id: projectId },
      body: {
        name,
        monitor_type: monitorType,
        check_interval_seconds: checkInterval,
        environment_id: environmentId,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Monitor "${name}" created successfully`)
  info('The monitor will start checking immediately')
}

async function showMonitor(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid monitor ID')
    return
  }

  const monitor = await withSpinner('Fetching monitor...', async () => {
    const { data, error } = await getMonitor({
      client,
      path: { monitor_id: id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Monitor ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(monitor)
    return
  }

  newline()
  header(`${icons.info} ${monitor.name}`)
  keyValue('ID', monitor.id)
  keyValue('Type', monitor.monitor_type)
  keyValue('URL', monitor.monitor_url)
  keyValue('Check Interval', `${monitor.check_interval_seconds} seconds`)
  keyValue('Status', monitor.is_active ? colors.success('Active') : colors.muted('Paused'))
  keyValue('Project ID', monitor.project_id)
  keyValue('Created', new Date(monitor.created_at).toLocaleString())
  keyValue('Updated', new Date(monitor.updated_at).toLocaleString())
  newline()
}

async function removeMonitor(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid monitor ID')
    return
  }

  // Get monitor details first
  const { data: monitor, error: getError } = await getMonitor({
    client,
    path: { monitor_id: id },
  })

  if (getError || !monitor) {
    warning(`Monitor ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete monitor "${monitor.name}"?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting monitor...', async () => {
    const { error } = await deleteMonitor({
      client,
      path: { monitor_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Monitor deleted')
}

async function getMonitorStatus(options: StatusOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid monitor ID')
    return
  }

  const status = await withSpinner('Fetching status...', async () => {
    const { data, error } = await getCurrentMonitorStatus({
      client,
      path: { monitor_id: id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to get monitor status')
    }
    return data
  })

  if (options.json) {
    json(status)
    return
  }

  newline()
  header(`${icons.info} Monitor Status`)
  keyValue('Current Status', statusBadge(status.current_status === 'up' ? 'active' : 'inactive'))
  if (status.avg_response_time_ms !== null && status.avg_response_time_ms !== undefined) {
    keyValue('Avg Response Time', `${Math.round(status.avg_response_time_ms)}ms`)
  }
  if (status.uptime_percentage !== null && status.uptime_percentage !== undefined) {
    const uptimeColor = status.uptime_percentage >= 99 ? colors.success : status.uptime_percentage >= 95 ? colors.warning : colors.error
    keyValue('Uptime', uptimeColor(`${status.uptime_percentage.toFixed(2)}%`))
  }
  if (status.last_check_at) {
    keyValue('Last Check', new Date(status.last_check_at).toLocaleString())
  }
  newline()
}

async function getMonitorHistory(options: HistoryOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid monitor ID')
    return
  }

  const days = parseInt(options.days || '7', 10)

  // Calculate start_time and end_time based on days
  const endTime = new Date()
  const startTime = new Date()
  startTime.setDate(startTime.getDate() - days)

  const history = await withSpinner('Fetching history...', async () => {
    const { data, error } = await getUptimeHistory({
      client,
      path: { monitor_id: id },
      query: {
        days,
        start_time: startTime.toISOString(),
        end_time: endTime.toISOString(),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(history)
    return
  }

  newline()
  header(`${icons.info} Uptime History (Last ${days} days)`)

  if (!history || !history.uptime_data || history.uptime_data.length === 0) {
    info('No history data available')
    newline()
    return
  }

  // Show data points
  for (const entry of history.uptime_data) {
    const statusIcon = entry.status === 'up' ? colors.success('●') : colors.error('●')
    const date = new Date(entry.timestamp).toLocaleString()
    const responseTime = entry.response_time_ms ? `${entry.response_time_ms}ms` : 'N/A'
    console.log(`  ${statusIcon} ${date}: ${entry.status} (${responseTime})`)
  }
  newline()
}
