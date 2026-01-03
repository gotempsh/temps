import { requireAuth, config } from '../../config/store.js'
import { setupClient, client } from '../../lib/api-client.js'
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

export async function logs(options: LogsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectName = options.project ?? config.get('defaultProject')

  if (!projectName) {
    warning('No project specified. Use: temps logs --project <project>')
    return
  }

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

    const logs = (Array.isArray(data) ? data : [data]) as LogEntry[]
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
          const logs = (Array.isArray(data) ? data : [data]) as LogEntry[]
          const lastLine = lastLines[job.id] || 0

          for (const log of logs) {
            if (log.line && log.line > lastLine) {
              console.log(colors.muted(`[${job.name}]`), formatLogMessage(log))
              lastLines[job.id] = log.line
            }
          }

          // If no line numbers, just print new logs based on count
          if (logs.length > 0 && logs[0]?.line) {
            const newLogs = logs.slice(lastLine)
            for (const log of newLogs) {
              console.log(colors.muted(`[${job.name}]`), formatLogMessage(log))
            }
            lastLines[job.id] = logs.length
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

function formatLogMessage(log: LogEntry): string {
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
