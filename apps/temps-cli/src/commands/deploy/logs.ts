import { requireAuth } from '../../config/store.js'
import { setupClient, client } from '../../lib/api-client.js'
import { resolveProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getProjectDeployments,
  getDeploymentJobs,
  getDeploymentJobLogs,
} from '../../api/sdk.gen.js'
import type { DeploymentJobResponse } from '../../api/types.gen.js'
import { startSpinner, succeedSpinner, failSpinner } from '../../ui/spinner.js'
import { newline, colors, info, warning } from '../../ui/output.js'

interface LogsOptions {
  project?: string
  environment: string
  follow?: boolean
  lines: string
  deployment?: string
}

interface LogEntry {
  timestamp?: string
  level?: string
  message: string
  line?: number
}

/**
 * Parse JSONL log content (string) into LogEntry[].
 * The API returns raw file content where each line is a JSON object.
 * Falls back to treating each line as a plain-text message if parsing fails.
 */
export function parseLogEntries(data: unknown): LogEntry[] {
  if (Array.isArray(data)) {
    return data as LogEntry[]
  }

  if (typeof data !== 'string') {
    return []
  }

  const lines = data.split('\n').filter(line => line.trim() !== '')
  return lines.map((line, index) => {
    try {
      const parsed = JSON.parse(line)
      return {
        timestamp: parsed.timestamp,
        level: parsed.level,
        message: parsed.message ?? line,
        line: parsed.line ?? (index + 1),
      } as LogEntry
    } catch {
      // Plain text line — treat as info-level message
      return { message: line, line: index + 1 } as LogEntry
    }
  })
}

export async function logs(options: LogsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await resolveProjectSlug(options.project)

  if (!resolved) {
    warning('No project specified')
    info('Use: bunx @temps-sdk/cli deployments logs --project <slug>')
    info('Or link this directory: bunx @temps-sdk/cli link <slug>')
    return
  }

  const projectName = resolved.slug

  // Get project ID
  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: projectName },
  })

  if (projectError || !projectData) {
    warning(`Project "${projectName}" not found`)
    return
  }

  // Get deployment ID
  let deploymentId = options.deployment ? parseInt(options.deployment, 10) : undefined

  if (!deploymentId) {
    startSpinner('Finding latest deployment...')

    try {
      const { data, error } = await getProjectDeployments({
        client,
        path: { id: projectData.id },
      })

      if (error || !data) {
        failSpinner('No deployments found')
        return
      }

      // Filter by environment if specified
      const deployments = data.deployments
        .filter(d => !options.environment || d.environment?.name === options.environment)

      if (deployments.length === 0) {
        failSpinner('No deployments found')
        return
      }

      deploymentId = deployments[0]!.id
      succeedSpinner(`Found deployment #${deploymentId}`)
    } catch (err) {
      failSpinner('Failed to find deployment')
      throw err
    }
  }

  newline()
  info(`${colors.muted('Showing logs for deployment')} #${deploymentId}`)
  newline()

  // Get jobs for this deployment
  const { data: jobs, error: jobsError } = await getDeploymentJobs({
    client,
    path: {
      project_id: projectData.id,
      deployment_id: deploymentId,
    },
  })

  const jobsArray = jobs?.jobs ?? []

  if (jobsError || jobsArray.length === 0) {
    warning('No jobs found for this deployment')
    return
  }

  const jobList = jobsArray

  if (options.follow) {
    await streamLogs(projectData.id, deploymentId, jobList)
  } else {
    await fetchLogs(projectData.id, deploymentId, jobList, parseInt(options.lines, 10))
  }
}

async function fetchLogs(
  projectId: number,
  deploymentId: number,
  jobs: DeploymentJobResponse[],
  limit: number
): Promise<void> {
  for (const job of jobs) {
    console.log(colors.bold(`\n=== ${job.name} ===\n`))

    const { data, error } = await getDeploymentJobLogs({
      client,
      path: {
        project_id: projectId,
        deployment_id: deploymentId,
        job_id: job.job_id,
      },
    })

    if (error || !data) {
      console.log(colors.muted('No logs available for this job'))
      continue
    }

    const logs = parseLogEntries(data)
    const limitedLogs = logs.slice(-limit)

    for (const log of limitedLogs) {
      printLogLine(log)
    }
  }
}

async function streamLogs(
  projectId: number,
  deploymentId: number,
  jobs: DeploymentJobResponse[]
): Promise<void> {
  info('Streaming logs (Ctrl+C to stop)...')
  newline()

  const lastLines: Record<number, number> = {}

  // Simple polling for logs
  // eslint-disable-next-line no-constant-condition
  while (true) {
    for (const job of jobs) {
      try {
        const { data } = await getDeploymentJobLogs({
          client,
          path: {
            project_id: projectId,
            deployment_id: deploymentId,
            job_id: job.job_id,
          },
        })

        if (data) {
          const logs = parseLogEntries(data)
          const lastLine = lastLines[job.id] || 0

          for (const log of logs) {
            if (log.line && log.line > lastLine) {
              console.log(colors.muted(`[${job.name}]`), formatLogMessage(log))
              lastLines[job.id] = log.line
            }
          }
        }
      } catch {
        // Ignore errors in streaming mode
      }
    }

    await new Promise((resolve) => setTimeout(resolve, 1000))
  }
}

function printLogLine(log: LogEntry): void {
  console.log(formatLogMessage(log))
}

export function formatLogMessage(log: LogEntry): string {
  const levelColors: Record<string, (s: string) => string> = {
    info: colors.info,
    success: colors.success,
    warning: colors.warning,
    error: colors.error,
  }

  const colorFn = log.level ? (levelColors[log.level] ?? colors.muted) : (s: string) => s
  const timestamp = log.timestamp
    ? colors.muted(new Date(log.timestamp).toLocaleTimeString()) + ' '
    : ''

  return `${timestamp}${colorFn(log.message)}`
}
