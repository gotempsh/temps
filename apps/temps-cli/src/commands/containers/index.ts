import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listContainers,
  getContainerDetail,
  startContainer,
  stopContainer,
  restartContainer,
  getContainerMetrics,
} from '../../api/sdk.gen.js'
import type { ContainerInfoResponse, ContainerDetailResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface ContainerOptions {
  projectId: string
  environmentId: string
  containerId?: string
  json?: boolean
  force?: boolean
}

export function registerContainersCommands(program: Command): void {
  const containers = program
    .command('containers')
    .alias('cts')
    .description('Manage project containers in environments')

  containers
    .command('list')
    .alias('ls')
    .description('List all containers in an environment')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-e, --environment-id <id>', 'Environment ID')
    .option('--json', 'Output in JSON format')
    .action(listContainersAction)

  containers
    .command('show')
    .description('Show container details')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-e, --environment-id <id>', 'Environment ID')
    .requiredOption('-c, --container-id <id>', 'Container ID')
    .option('--json', 'Output in JSON format')
    .action(showContainer)

  containers
    .command('start')
    .description('Start a stopped container')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-e, --environment-id <id>', 'Environment ID')
    .requiredOption('-c, --container-id <id>', 'Container ID')
    .action(startContainerAction)

  containers
    .command('stop')
    .description('Stop a running container')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-e, --environment-id <id>', 'Environment ID')
    .requiredOption('-c, --container-id <id>', 'Container ID')
    .option('-f, --force', 'Skip confirmation')
    .action(stopContainerAction)

  containers
    .command('restart')
    .description('Restart a container')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-e, --environment-id <id>', 'Environment ID')
    .requiredOption('-c, --container-id <id>', 'Container ID')
    .action(restartContainerAction)

  containers
    .command('metrics')
    .description('Get container resource metrics (all containers if no container ID specified)')
    .requiredOption('-p, --project-id <id>', 'Project ID')
    .requiredOption('-e, --environment-id <id>', 'Environment ID')
    .option('-c, --container-id <id>', 'Container ID (optional - shows all if not specified)')
    .option('--json', 'Output in JSON format')
    .option('-w, --watch', 'Watch mode - continuously update metrics')
    .option('-i, --interval <seconds>', 'Refresh interval in seconds (default: 2)', '2')
    .action(getContainerMetricsAction)
}

async function listContainersAction(
  options: { projectId: string; environmentId: string; json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const envId = parseInt(options.environmentId, 10)
  if (isNaN(projId) || isNaN(envId)) {
    warning('Invalid project or environment ID')
    return
  }

  const result = await withSpinner('Fetching containers...', async () => {
    const { data, error } = await listContainers({
      client,
      path: { project_id: projId, environment_id: envId },
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
  header(`${icons.info} Containers in Environment ${envId} (${result?.total ?? 0})`)

  if (!result?.containers || result.containers.length === 0) {
    info('No containers found in this environment')
    newline()
    return
  }

  const columns: TableColumn<ContainerInfoResponse>[] = [
    { header: 'ID', key: 'container_id', width: 16, color: (v) => colors.muted(v.slice(0, 12) + '...') },
    { header: 'Name', key: 'container_name', color: (v) => colors.bold(v) },
    { header: 'Image', key: 'image_name', color: (v) => colors.muted(v.length > 40 ? v.slice(0, 40) + '...' : v) },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v.toLowerCase().includes('running') ? 'active' : 'inactive') },
    { header: 'Created', key: 'created_at', color: (v) => colors.muted(new Date(v).toLocaleDateString()) },
  ]

  printTable(result.containers, columns, { style: 'minimal' })
  newline()
}

async function showContainer(
  options: { projectId: string; environmentId: string; containerId: string; json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const envId = parseInt(options.environmentId, 10)
  if (isNaN(projId) || isNaN(envId)) {
    warning('Invalid project or environment ID')
    return
  }

  const container = await withSpinner('Fetching container details...', async () => {
    const { data, error } = await getContainerDetail({
      client,
      path: { project_id: projId, environment_id: envId, container_id: options.containerId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Container ${options.containerId} not found`)
    }
    return data
  })

  if (options.json) {
    json(container)
    return
  }

  newline()
  header(`${icons.info} ${container.container_name}`)
  keyValue('Container ID', container.container_id)
  keyValue('Image', container.image_name)
  keyValue('Status', statusBadge(container.status.toLowerCase().includes('running') ? 'active' : 'inactive'))
  keyValue('Container Port', container.container_port)
  if (container.host_port) {
    keyValue('Host Port', container.host_port)
  }
  keyValue('Deployment ID', container.deployment_id)
  keyValue('Created', new Date(container.created_at).toLocaleString())
  keyValue('Deployed', new Date(container.deployed_at).toLocaleString())
  if (container.ready_at) {
    keyValue('Ready', new Date(container.ready_at).toLocaleString())
  }

  if (container.resource_limits) {
    newline()
    header('Resource Limits')
    if (container.resource_limits.cpu_limit) {
      keyValue('CPU Limit', container.resource_limits.cpu_limit)
    }
    if (container.resource_limits.memory_limit) {
      keyValue('Memory Limit', container.resource_limits.memory_limit)
    }
  }

  if (container.environment_variables && container.environment_variables.length > 0) {
    newline()
    header('Environment Variables')
    for (const envVar of container.environment_variables) {
      console.log(`  ${colors.bold(envVar.key)}: ${colors.muted(envVar.value)}`)
    }
  }
  newline()
}

async function startContainerAction(
  options: { projectId: string; environmentId: string; containerId: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const envId = parseInt(options.environmentId, 10)
  if (isNaN(projId) || isNaN(envId)) {
    warning('Invalid project or environment ID')
    return
  }

  const result = await withSpinner('Starting container...', async () => {
    const { data, error } = await startContainer({
      client,
      path: { project_id: projId, environment_id: envId, container_id: options.containerId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Container started: ${result?.container_name ?? options.containerId}`)
  info(`Status: ${result?.status}`)
}

async function stopContainerAction(
  options: { projectId: string; environmentId: string; containerId: string; force?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const envId = parseInt(options.environmentId, 10)
  if (isNaN(projId) || isNaN(envId)) {
    warning('Invalid project or environment ID')
    return
  }

  if (!options.force) {
    const confirmed = await promptConfirm({
      message: `Stop container ${options.containerId.slice(0, 12)}...?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const result = await withSpinner('Stopping container...', async () => {
    const { data, error } = await stopContainer({
      client,
      path: { project_id: projId, environment_id: envId, container_id: options.containerId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Container stopped: ${result?.container_name ?? options.containerId}`)
  info(`Status: ${result?.status}`)
}

async function restartContainerAction(
  options: { projectId: string; environmentId: string; containerId: string }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const envId = parseInt(options.environmentId, 10)
  if (isNaN(projId) || isNaN(envId)) {
    warning('Invalid project or environment ID')
    return
  }

  const result = await withSpinner('Restarting container...', async () => {
    const { data, error } = await restartContainer({
      client,
      path: { project_id: projId, environment_id: envId, container_id: options.containerId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Container restarted: ${result?.container_name ?? options.containerId}`)
  info(`Status: ${result?.status}`)
}

interface MetricsOptions {
  projectId: string
  environmentId: string
  containerId?: string
  json?: boolean
  watch?: boolean
  interval?: string
}

interface ContainerMetrics {
  container_id: string
  container_name: string
  timestamp: string
  cpu_percent: number
  memory_bytes: number
  memory_limit_bytes?: number | null
  memory_percent?: number | null
  network_rx_bytes: number
  network_tx_bytes: number
}

async function getContainerMetricsAction(options: MetricsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const envId = parseInt(options.environmentId, 10)
  if (isNaN(projId) || isNaN(envId)) {
    warning('Invalid project or environment ID')
    return
  }

  const intervalMs = (parseInt(options.interval ?? '2', 10) || 2) * 1000

  // If specific container ID is provided, use single container mode
  if (options.containerId) {
    if (options.watch) {
      await watchMetrics(projId, envId, [options.containerId], intervalMs)
      return
    }

    const metrics = await withSpinner('Fetching container metrics...', async () => {
      const { data, error } = await getContainerMetrics({
        client,
        path: { project_id: projId, environment_id: envId, container_id: options.containerId! },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data
    })

    if (options.json) {
      json(metrics)
      return
    }

    if (!metrics) {
      info('No metrics available for this container')
      return
    }

    displayMetrics(metrics)
    return
  }

  // No container ID specified - get metrics for all containers
  const containers = await withSpinner('Fetching containers...', async () => {
    const { data, error } = await listContainers({
      client,
      path: { project_id: projId, environment_id: envId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data?.containers ?? []
  })

  if (containers.length === 0) {
    info('No containers found in this environment')
    return
  }

  const containerIds = containers.map(c => c.container_id)

  if (options.watch) {
    await watchMetrics(projId, envId, containerIds, intervalMs)
    return
  }

  // Fetch metrics for all containers
  const allMetrics = await withSpinner(`Fetching metrics for ${containers.length} containers...`, async () => {
    const metricsPromises = containers.map(async (container) => {
      try {
        const { data, error } = await getContainerMetrics({
          client,
          path: { project_id: projId, environment_id: envId, container_id: container.container_id },
        })
        if (error || !data) return null
        return { ...data, container_id: container.container_id }
      } catch {
        return null
      }
    })
    return (await Promise.all(metricsPromises)).filter((m): m is ContainerMetrics => m !== null)
  })

  if (options.json) {
    json(allMetrics)
    return
  }

  if (allMetrics.length === 0) {
    info('No metrics available for containers in this environment')
    return
  }

  displayAllContainerMetrics(allMetrics)
}

function displayMetrics(metrics: {
  container_name: string
  timestamp: string
  cpu_percent: number
  memory_bytes: number
  memory_limit_bytes?: number | null
  memory_percent?: number | null
  network_rx_bytes: number
  network_tx_bytes: number
}): void {
  newline()
  header(`${icons.info} Container Metrics`)
  keyValue('Container', metrics.container_name)
  keyValue('Timestamp', new Date(metrics.timestamp).toLocaleString())

  newline()
  header('CPU Usage')
  keyValue('CPU %', `${metrics.cpu_percent.toFixed(2)}%`)

  newline()
  header('Memory Usage')
  const memoryUsageMB = (metrics.memory_bytes / 1024 / 1024).toFixed(2)
  const memoryLimitMB = metrics.memory_limit_bytes ? (metrics.memory_limit_bytes / 1024 / 1024).toFixed(2) : 'N/A'
  keyValue('Usage', `${memoryUsageMB} MB`)
  keyValue('Limit', `${memoryLimitMB} MB`)
  if (metrics.memory_percent != null) {
    keyValue('Memory %', `${metrics.memory_percent.toFixed(2)}%`)
  }

  newline()
  header('Network I/O')
  const netRxMB = (metrics.network_rx_bytes / 1024 / 1024).toFixed(2)
  const netTxMB = (metrics.network_tx_bytes / 1024 / 1024).toFixed(2)
  keyValue('Received', `${netRxMB} MB`)
  keyValue('Transmitted', `${netTxMB} MB`)

  newline()
}

function displayAllContainerMetrics(allMetrics: ContainerMetrics[]): void {
  newline()
  header(`${icons.info} Container Metrics (${allMetrics.length} containers)`)
  newline()

  // Create a table-like display for all containers
  const columns: TableColumn<ContainerMetrics>[] = [
    {
      header: 'Container',
      accessor: (m) => m.container_name.length > 25 ? m.container_name.slice(0, 22) + '...' : m.container_name,
      color: (v) => colors.bold(v),
    },
    {
      header: 'CPU %',
      accessor: (m) => m.cpu_percent.toFixed(1) + '%',
      color: (v, m) => {
        const pct = m.cpu_percent
        return pct > 90 ? colors.error(v) : pct > 70 ? colors.warning(v) : colors.success(v)
      },
    },
    {
      header: 'Memory',
      accessor: (m) => {
        const used = formatBytes(m.memory_bytes)
        const limit = m.memory_limit_bytes ? formatBytes(m.memory_limit_bytes) : 'N/A'
        return `${used} / ${limit}`
      },
    },
    {
      header: 'Mem %',
      accessor: (m) => (m.memory_percent ?? 0).toFixed(1) + '%',
      color: (v, m) => {
        const pct = m.memory_percent ?? 0
        return pct > 90 ? colors.error(v) : pct > 70 ? colors.warning(v) : colors.success(v)
      },
    },
    {
      header: 'Net ↓',
      accessor: (m) => formatBytes(m.network_rx_bytes),
      color: (v) => colors.success(v),
    },
    {
      header: 'Net ↑',
      accessor: (m) => formatBytes(m.network_tx_bytes),
      color: (v) => colors.warning(v),
    },
  ]

  printTable(allMetrics, columns, { style: 'minimal' })
  newline()
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`
}

function createProgressBar(percent: number, width: number = 30): string {
  const filled = Math.round((percent / 100) * width)
  const empty = width - filled
  const bar = '█'.repeat(filled) + '░'.repeat(empty)
  const color = percent > 90 ? colors.error : percent > 70 ? colors.warning : colors.success
  return color(bar)
}

async function watchMetrics(
  projectId: number,
  environmentId: number,
  containerIds: string[],
  intervalMs: number
): Promise<void> {
  let isRunning = true

  // Track network stats per container for rate calculations
  const networkStats: Map<string, { rx: number; tx: number; timestamp: number }> = new Map()

  // Handle Ctrl+C gracefully
  process.on('SIGINT', () => {
    isRunning = false
    console.log('\x1b[?25h') // Show cursor
    console.log('\n')
    console.log(colors.muted('Stopped watching metrics'))
    process.exit(0)
  })

  // Clear screen and hide cursor
  console.log('\x1b[?25l') // Hide cursor
  process.stdout.write('\x1b[2J\x1b[H') // Clear screen

  const isSingleContainer = containerIds.length === 1

  while (isRunning) {
    try {
      // Fetch metrics for all containers
      const metricsPromises = containerIds.map(async (containerId) => {
        try {
          const { data, error } = await getContainerMetrics({
            client,
            path: { project_id: projectId, environment_id: environmentId, container_id: containerId },
          })
          if (error || !data) return null
          return { ...data, container_id: containerId }
        } catch {
          return null
        }
      })

      const allMetrics = (await Promise.all(metricsPromises)).filter((m): m is ContainerMetrics => m !== null)

      // Move cursor to top and clear screen
      process.stdout.write('\x1b[H\x1b[2J')

      if (allMetrics.length === 0) {
        console.log(colors.warning('No metrics available'))
        await new Promise((resolve) => setTimeout(resolve, intervalMs))
        continue
      }

      const now = Date.now()

      if (isSingleContainer && allMetrics[0]) {
        // Single container: detailed view with progress bars
        const metrics = allMetrics[0]
        const lastStats = networkStats.get(metrics.container_id)
        const timeDelta = lastStats ? (now - lastStats.timestamp) / 1000 : 0
        const rxRate = lastStats && timeDelta > 0 ? (metrics.network_rx_bytes - lastStats.rx) / timeDelta : 0
        const txRate = lastStats && timeDelta > 0 ? (metrics.network_tx_bytes - lastStats.tx) / timeDelta : 0

        networkStats.set(metrics.container_id, {
          rx: metrics.network_rx_bytes,
          tx: metrics.network_tx_bytes,
          timestamp: now,
        })

        // Header
        console.log(colors.bold(`╔${'═'.repeat(58)}╗`))
        console.log(colors.bold(`║  ${icons.info} Container Metrics - ${metrics.container_name.slice(0, 30).padEnd(30)}  ║`))
        console.log(colors.bold(`╠${'═'.repeat(58)}╣`))

        // Timestamp
        const timestamp = new Date(metrics.timestamp).toLocaleTimeString()
        console.log(colors.bold(`║`) + `  Last Updated: ${colors.primary(timestamp)}`.padEnd(67) + colors.bold(`║`))
        console.log(colors.bold(`╠${'═'.repeat(58)}╣`))

        // CPU Usage
        const cpuPercent = metrics.cpu_percent
        console.log(colors.bold(`║`) + colors.primary('  CPU Usage').padEnd(67) + colors.bold(`║`))
        console.log(colors.bold(`║`) + `  ${createProgressBar(cpuPercent)} ${cpuPercent.toFixed(1).padStart(5)}%`.padEnd(60) + colors.bold(`║`))
        console.log(colors.bold(`╠${'═'.repeat(58)}╣`))

        // Memory Usage
        const memPercent = metrics.memory_percent ?? 0
        const memUsage = formatBytes(metrics.memory_bytes)
        const memLimit = metrics.memory_limit_bytes ? formatBytes(metrics.memory_limit_bytes) : 'N/A'
        console.log(colors.bold(`║`) + colors.primary('  Memory Usage').padEnd(67) + colors.bold(`║`))
        console.log(colors.bold(`║`) + `  ${createProgressBar(memPercent)} ${memPercent.toFixed(1).padStart(5)}%`.padEnd(60) + colors.bold(`║`))
        console.log(colors.bold(`║`) + `  ${memUsage} / ${memLimit}`.padEnd(59) + colors.bold(`║`))
        console.log(colors.bold(`╠${'═'.repeat(58)}╣`))

        // Network I/O
        console.log(colors.bold(`║`) + colors.primary('  Network I/O').padEnd(67) + colors.bold(`║`))
        const rxFormatted = `↓ ${formatBytes(metrics.network_rx_bytes)}`
        const txFormatted = `↑ ${formatBytes(metrics.network_tx_bytes)}`
        const rxRateStr = rxRate > 0 ? ` (${formatBytes(rxRate)}/s)` : ''
        const txRateStr = txRate > 0 ? ` (${formatBytes(txRate)}/s)` : ''
        console.log(colors.bold(`║`) + `  ${colors.success(rxFormatted)}${colors.muted(rxRateStr)}`.padEnd(67) + colors.bold(`║`))
        console.log(colors.bold(`║`) + `  ${colors.warning(txFormatted)}${colors.muted(txRateStr)}`.padEnd(67) + colors.bold(`║`))
        console.log(colors.bold(`╚${'═'.repeat(58)}╝`))
      } else {
        // Multiple containers: table view
        console.log(colors.bold(`╔${'═'.repeat(90)}╗`))
        const titleLine = `  ${icons.info} Container Metrics - ${allMetrics.length} containers`
        console.log(colors.bold('║') + titleLine.padEnd(90) + colors.bold('║'))
        const updatedLine = `  Last Updated: ${new Date().toLocaleTimeString()}`
        console.log(colors.bold('║') + updatedLine.padEnd(90) + colors.bold('║'))
        console.log(colors.bold(`╠${'═'.repeat(90)}╣`))

        // Table header
        const headerRow = [
          'Container'.padEnd(28),
          'CPU %'.padStart(8),
          'Mem %'.padStart(8),
          'Memory'.padEnd(18),
          'Net RX'.padEnd(12),
          'Net TX'.padEnd(12),
        ].join(' | ')
        console.log(colors.bold('║ ') + colors.primary(headerRow) + colors.bold(' ║'))
        console.log(colors.bold(`╠${'═'.repeat(90)}╣`))

        // Table rows
        for (const metrics of allMetrics) {
          const containerName = metrics.container_name.length > 26
            ? metrics.container_name.slice(0, 23) + '...'
            : metrics.container_name.padEnd(28)

          const cpuStr = metrics.cpu_percent.toFixed(1) + '%'
          const cpuColor = metrics.cpu_percent > 90 ? colors.error : metrics.cpu_percent > 70 ? colors.warning : colors.success

          const memPct = metrics.memory_percent ?? 0
          const memPctStr = memPct.toFixed(1) + '%'
          const memColor = memPct > 90 ? colors.error : memPct > 70 ? colors.warning : colors.success

          const memUsage = formatBytes(metrics.memory_bytes)
          const memLimit = metrics.memory_limit_bytes ? formatBytes(metrics.memory_limit_bytes) : 'N/A'
          const memStr = `${memUsage}/${memLimit}`

          const row = [
            colors.bold(containerName),
            cpuColor(cpuStr.padStart(8)),
            memColor(memPctStr.padStart(8)),
            memStr.padEnd(18),
            colors.success(formatBytes(metrics.network_rx_bytes).padEnd(12)),
            colors.warning(formatBytes(metrics.network_tx_bytes).padEnd(12)),
          ].join(' | ')

          console.log(colors.bold('║ ') + row + colors.bold(' ║'))
        }

        console.log(colors.bold(`╚${'═'.repeat(90)}╝`))
      }

      // Footer
      console.log()
      console.log(colors.muted(`  Press Ctrl+C to stop watching (refresh: ${intervalMs / 1000}s)`))

    } catch (err) {
      console.log(colors.error(`Error fetching metrics: ${err}`))
    }

    await new Promise((resolve) => setTimeout(resolve, intervalMs))
  }

  // Show cursor again
  console.log('\x1b[?25h')
}
