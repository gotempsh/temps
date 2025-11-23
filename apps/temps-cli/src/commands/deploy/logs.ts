import { requireAuth, config } from '../../config/store.js'
import { startSpinner, succeedSpinner, failSpinner } from '../../ui/spinner.js'
import { newline, colors, info, warning } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface LogsOptions {
  environment: string
  follow?: boolean
  lines: string
  deployment?: string
}

interface LogEntry {
  timestamp: string
  level: string
  message: string
  line?: number
}

export async function logs(project: string, options: LogsOptions): Promise<void> {
  await requireAuth()

  const projectName = project ?? config.get('defaultProject')

  if (!projectName) {
    warning('No project specified')
    return
  }

  const client = getClient()

  // Get deployment ID
  let deploymentId = options.deployment

  if (!deploymentId) {
    startSpinner('Finding latest deployment...')

    try {
      const response = await client.get('/api/projects/{project}/deployments' as never, {
        params: {
          path: { project: projectName },
          query: {
            environment: options.environment,
            limit: 1,
          },
        },
      })

      if (response.error || !response.data) {
        failSpinner('No deployments found')
        return
      }

      const deployments = response.data as Array<{ id: number }>
      if (deployments.length === 0) {
        failSpinner('No deployments found')
        return
      }

      deploymentId = String(deployments[0].id)
      succeedSpinner(`Found deployment #${deploymentId}`)
    } catch (err) {
      failSpinner('Failed to find deployment')
      throw err
    }
  }

  newline()
  info(`${colors.muted('Showing logs for deployment')} #${deploymentId}`)
  newline()

  if (options.follow) {
    await streamLogs(client, deploymentId)
  } else {
    await fetchLogs(client, deploymentId, parseInt(options.lines, 10))
  }
}

async function fetchLogs(
  client: ReturnType<typeof getClient>,
  deploymentId: string,
  limit: number
): Promise<void> {
  const response = await client.get('/api/deployments/{id}/logs' as never, {
    params: {
      path: { id: deploymentId },
      query: { limit },
    },
  })

  if (response.error) {
    throw new Error('Failed to fetch logs')
  }

  const logs = (response.data ?? []) as LogEntry[]

  for (const log of logs) {
    printLogLine(log)
  }
}

async function streamLogs(
  client: ReturnType<typeof getClient>,
  deploymentId: string
): Promise<void> {
  info('Streaming logs (Ctrl+C to stop)...')
  newline()

  let lastLine = 0

  // Simple polling for logs (would be better with SSE/WebSocket)
  // eslint-disable-next-line no-constant-condition
  while (true) {
    try {
      const response = await client.get('/api/deployments/{id}/logs' as never, {
        params: {
          path: { id: deploymentId },
          query: { after_line: lastLine },
        },
      })

      if (response.data) {
        const logs = response.data as LogEntry[]
        for (const log of logs) {
          printLogLine(log)
          if (log.line && log.line > lastLine) {
            lastLine = log.line
          }
        }
      }
    } catch {
      // Ignore errors in streaming mode
    }

    await new Promise((resolve) => setTimeout(resolve, 1000))
  }
}

function printLogLine(log: LogEntry): void {
  const levelColors: Record<string, (s: string) => string> = {
    info: colors.info,
    success: colors.success,
    warning: colors.warning,
    error: colors.error,
  }

  const colorFn = levelColors[log.level] ?? colors.muted
  const timestamp = colors.muted(new Date(log.timestamp).toLocaleTimeString())

  console.log(`${timestamp} ${colorFn(log.message)}`)
}
