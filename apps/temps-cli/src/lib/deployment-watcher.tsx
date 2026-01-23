import { useState, useEffect } from 'react'
import { render, Box, Text, Newline } from 'ink'
import Spinner from 'ink-spinner'
import { config, credentials } from '../config/store.js'

interface DeploymentEnvironment {
  id: number
  name: string
  slug: string
  domains: string[]
}

interface DeploymentResponse {
  id: number
  slug?: string
  status: string
  url?: string
  cancelled_reason?: string
  environment?: DeploymentEnvironment
}

interface DeploymentJobResponse {
  id: number
  job_id: string
  name: string
  status: string
  error_message?: string | null
  log_id: string
  started_at?: number | null
  finished_at?: number | null
}

interface LogEntry {
  level: string
  message: string
  timestamp: string
  line: number
}

interface WatchDeploymentOptions {
  projectId: number
  deploymentId: number
  timeoutSecs: number
  projectName?: string
}

interface WatcherInternalProps extends WatchDeploymentOptions {
  apiUrl: string
  apiKey: string
  onComplete: (result: WatchDeploymentResult) => void
}

interface WatchDeploymentResult {
  success: boolean
  deployment?: DeploymentResponse
  error?: string
}

interface JobState {
  job: DeploymentJobResponse
  logs: LogEntry[]
  lastLogLine: number
}

// Convert API timestamp to milliseconds
function toMs(timestamp: number): number {
  if (timestamp < 946684800000) {
    return timestamp * 1000
  }
  return timestamp
}

function formatDuration(startTimestamp: number, endTimestamp?: number): string {
  const startMs = toMs(startTimestamp)
  const endMs = endTimestamp ? toMs(endTimestamp) : Date.now()
  const duration = endMs - startMs

  if (duration < 0 || duration > 86400000) {
    return ''
  }

  const seconds = Math.floor(duration / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  const remainingSeconds = seconds % 60
  return `${minutes}m ${remainingSeconds}s`
}

// Status icon component
function StatusIcon({ status }: { status: string }) {
  switch (status) {
    case 'running':
      return <Text color="yellow"><Spinner type="dots" /></Text>
    case 'success':
    case 'completed':
    case 'deployed':
      return <Text color="green">●</Text>
    case 'failed':
    case 'error':
      return <Text color="red">✗</Text>
    case 'cancelled':
      return <Text color="gray">⊘</Text>
    default:
      return <Text color="gray">○</Text>
  }
}

// Status color helper
function getStatusColor(status: string): string {
  switch (status) {
    case 'running':
      return 'yellow'
    case 'success':
    case 'completed':
    case 'deployed':
      return 'green'
    case 'failed':
    case 'error':
      return 'red'
    default:
      return 'gray'
  }
}

// Log entry component
function LogEntryRow({ entry }: { entry: LogEntry }) {
  let color = 'gray'
  let icon = ' '

  switch (entry.level.toLowerCase()) {
    case 'success':
      color = 'green'
      icon = '✓'
      break
    case 'error':
      color = 'red'
      icon = '✗'
      break
    case 'warning':
      color = 'yellow'
      icon = '!'
      break
  }

  // Clean up the message
  const message = entry.message.replace(/^[✅❌⏳📦📋📂🚀📍🔄⬇️🐳🏷️]\s*/, '')

  return (
    <Box marginLeft={4}>
      <Text color={color}>{icon} {message}</Text>
    </Box>
  )
}

// Job row component
function JobRow({ jobState, showAllLogs }: { jobState: JobState; showAllLogs?: boolean }) {
  const { job, logs } = jobState
  const duration = job.started_at
    ? formatDuration(job.started_at, job.finished_at ?? undefined)
    : ''

  const statusColor = getStatusColor(job.status)
  // Show more logs for running jobs, fewer for completed
  const maxLogs = showAllLogs ? 20 : (job.status === 'running' ? 10 : 5)
  const recentLogs = logs.slice(-maxLogs)

  return (
    <Box flexDirection="column" marginLeft={2}>
      <Box>
        <StatusIcon status={job.status} />
        <Text color={statusColor}> {job.name}</Text>
        {duration && <Text color="gray"> ({duration})</Text>}
        {logs.length > 0 && <Text color="gray"> [{logs.length} logs]</Text>}
      </Box>

      {/* Error message */}
      {(job.status === 'failed' || job.status === 'error') && job.error_message && (
        <Box marginLeft={2}>
          <Text color="red">Error: {job.error_message}</Text>
        </Box>
      )}

      {/* Logs - always show if there are any */}
      {recentLogs.length > 0 && (
        <Box flexDirection="column" marginLeft={2} marginTop={0}>
          {recentLogs.map((log, i) => (
            <LogEntryRow key={`${job.job_id}-log-${i}`} entry={log} />
          ))}
        </Box>
      )}
    </Box>
  )
}

// Main deployment watcher component
function DeploymentWatcher({
  projectId,
  deploymentId,
  timeoutSecs,
  projectName: _projectName,
  apiUrl,
  apiKey,
  onComplete,
}: WatcherInternalProps) {
  const [deployment, setDeployment] = useState<DeploymentResponse | null>(null)
  const [jobStates, setJobStates] = useState<Map<string, JobState>>(new Map())
  const [startTime] = useState(Date.now())
  const [elapsed, setElapsed] = useState('0s')
  const [error, setError] = useState<string | null>(null)

  // Update elapsed time
  useEffect(() => {
    const timer = setInterval(() => {
      const seconds = Math.floor((Date.now() - startTime) / 1000)
      if (seconds < 60) {
        setElapsed(`${seconds}s`)
      } else {
        const minutes = Math.floor(seconds / 60)
        const remainingSeconds = seconds % 60
        setElapsed(`${minutes}m ${remainingSeconds}s`)
      }
    }, 1000)

    return () => clearInterval(timer)
  }, [startTime])

  // Main polling effect
  useEffect(() => {
    let cancelled = false
    const timeoutMs = timeoutSecs * 1000

    async function poll() {

      while (!cancelled && Date.now() - startTime < timeoutMs) {
        try {
          // Fetch deployment
          const deploymentRes = await fetch(
            `${apiUrl}/projects/${projectId}/deployments/${deploymentId}`,
            { headers: { Authorization: `Bearer ${apiKey}` } }
          )

          if (deploymentRes.ok) {
            const dep = (await deploymentRes.json()) as DeploymentResponse
            setDeployment(dep)

            // Check terminal states
            const isDeploymentTerminal = ['success', 'completed', 'deployed', 'failed', 'error', 'cancelled'].includes(dep.status)

            if (isDeploymentTerminal) {
              // For failed deployments, exit immediately
              if (['failed', 'error', 'cancelled'].includes(dep.status)) {
                onComplete({
                  success: false,
                  deployment: dep,
                  error: dep.cancelled_reason || 'Deployment failed',
                })
                return
              }

              // For successful deployments:
              // - If URL is available, deployment is live
              // - Otherwise check if "Mark Deployment Complete" job is done
              const markCompleteJob = Array.from(jobStates.values()).find(
                js => js.job.name.toLowerCase().includes('mark deployment complete')
              )
              const isMarkCompleteDone = markCompleteJob &&
                ['success', 'completed'].includes(markCompleteJob.job.status)

              if (dep.url || isMarkCompleteDone) {
                onComplete({ success: true, deployment: dep })
                return
              }
            }
          } else {
            // Show error in UI
            const errorText = await deploymentRes.text()
            setError(`API Error ${deploymentRes.status}: ${errorText.substring(0, 200)}`)
          }

          // Fetch jobs
          const jobsRes = await fetch(
            `${apiUrl}/projects/${projectId}/deployments/${deploymentId}/jobs`,
            { headers: { Authorization: `Bearer ${apiKey}` } }
          )

          if (jobsRes.ok) {
            const jobsData = (await jobsRes.json()) as { jobs: DeploymentJobResponse[] }
            const jobs = jobsData.jobs || []
            jobs.sort((a, b) => a.id - b.id)

            // Update job states and fetch logs
            const newJobStates = new Map(jobStates)

            for (const job of jobs) {
              let state = newJobStates.get(job.job_id)
              if (!state) {
                state = { job, logs: [], lastLogLine: 0 }
              } else {
                state = { ...state, job }
              }

              // Fetch logs for jobs that have started
              if (job.status !== 'pending' && job.status !== 'queued') {
                try {
                  const logsRes = await fetch(
                    `${apiUrl}/projects/${projectId}/deployments/${deploymentId}/jobs/${job.id}/logs`,
                    { headers: { Authorization: `Bearer ${apiKey}` } }
                  )

                  if (logsRes.ok) {
                    const logsText = await logsRes.text()
                    if (logsText.trim()) {
                      const newLogs: LogEntry[] = []
                      for (const line of logsText.trim().split('\n')) {
                        if (!line.trim()) continue
                        try {
                          const entry = JSON.parse(line) as LogEntry
                          if (entry.line > state.lastLogLine) {
                            newLogs.push(entry)
                            state.lastLogLine = entry.line
                          }
                        } catch {}
                      }
                      if (newLogs.length > 0) {
                        state = { ...state, logs: [...state.logs, ...newLogs] }
                      }
                    }
                  }
                } catch {}
              }

              newJobStates.set(job.job_id, state)
            }

            setJobStates(newJobStates)
          }

          await new Promise((r) => setTimeout(r, 1000))
        } catch (err) {
          setError(`Exception: ${err instanceof Error ? err.message : String(err)}`)
          await new Promise((r) => setTimeout(r, 1000))
        }
      }

      // Timeout
      if (!cancelled) {
        onComplete({ success: false, error: 'Timeout' })
      }
    }

    poll()

    return () => {
      cancelled = true
    }
  }, [projectId, deploymentId, timeoutSecs, startTime, onComplete])

  const sortedJobs = Array.from(jobStates.values()).sort((a, b) => a.job.id - b.job.id)
  const statusColor = deployment ? getStatusColor(deployment.status) : 'gray'

  return (
    <Box flexDirection="column" paddingTop={1}>
      {/* Header */}
      <Box>
        <Text bold>{'  '}🚀 Deployment Progress</Text>
      </Box>

      <Newline />

      {/* Deployment status */}
      <Box marginLeft={1}>
        {deployment ? (
          <>
            <StatusIcon status={deployment.status} />
            <Text bold> Deployment </Text>
            <Text color={statusColor}>{deployment.status}</Text>
            <Text color="gray"> ({elapsed})</Text>
          </>
        ) : (
          <>
            <Text color="yellow"><Spinner type="dots" /></Text>
            <Text> Connecting...</Text>
          </>
        )}
      </Box>

      {/* Error display */}
      {error && (
        <Box marginLeft={1} marginTop={1}>
          <Text color="red">Error: {error}</Text>
        </Box>
      )}

      <Newline />

      {/* Jobs */}
      {sortedJobs.map((jobState) => (
        <JobRow key={jobState.job.job_id} jobState={jobState} />
      ))}

      {sortedJobs.length === 0 && deployment && (
        <Box marginLeft={2}>
          <Text color="gray">Waiting for jobs...</Text>
        </Box>
      )}

      <Newline />
    </Box>
  )
}

// Result display component
function DeploymentResult({
  result,
  projectName,
}: {
  result: WatchDeploymentResult
  projectName?: string
}) {
  if (result.success) {
    const deployment = result.deployment
    const envDomains = deployment?.environment?.domains || []
    // Domain might already include protocol, check before adding https://
    const firstDomain = envDomains[0]
    const envUrl = firstDomain
      ? (firstDomain.startsWith('http') ? firstDomain : `https://${firstDomain}`)
      : null

    return (
      <Box flexDirection="column" paddingTop={1}>
        <Box>
          <Text color="green">{'  '}✓ Deployment completed successfully!</Text>
        </Box>
        <Newline />
        <Box marginLeft={2}>
          <Text>Deployment ID: </Text>
          <Text bold>{deployment?.id}</Text>
        </Box>
        {deployment?.url && (
          <Box marginLeft={2}>
            <Text>Deployment URL: </Text>
            <Text color="cyan" bold>{deployment.url}</Text>
          </Box>
        )}
        {envUrl && (
          <Box marginLeft={2}>
            <Text>Environment URL: </Text>
            <Text color="cyan" bold>{envUrl}</Text>
          </Box>
        )}
        <Newline />
      </Box>
    )
  }

  return (
    <Box flexDirection="column" paddingTop={1}>
      <Box>
        <Text color="red">{'  '}✗ Deployment failed</Text>
      </Box>
      {result.error && (
        <Box marginLeft={2}>
          <Text color="red">Reason: {result.error}</Text>
        </Box>
      )}
      {projectName && (
        <Box marginLeft={2}>
          <Text color="gray">View full logs: temps logs {projectName}</Text>
        </Box>
      )}
      <Newline />
    </Box>
  )
}

/**
 * Watch a deployment with an Ink-based TUI
 */
export async function watchDeployment(
  options: WatchDeploymentOptions
): Promise<WatchDeploymentResult> {
  // Fetch credentials before rendering to avoid async issues in React
  const apiUrl = config.get('apiUrl')
  const apiKey = await credentials.getApiKey() || ''

  if (!apiKey) {
    return { success: false, error: 'No API key found. Please run: temps login' }
  }

  return new Promise((resolve) => {
    let instance: ReturnType<typeof render> | null = null

    const handleComplete = (res: WatchDeploymentResult) => {
      // Unmount the watcher and show the result
      if (instance) {
        instance.unmount()
      }

      // Render the result
      const resultInstance = render(
        <DeploymentResult result={res} projectName={options.projectName} />
      )

      // Wait a bit then unmount and resolve
      setTimeout(() => {
        resultInstance.unmount()
        resolve(res)
      }, 100)
    }

    instance = render(
      <DeploymentWatcher
        {...options}
        apiUrl={apiUrl}
        apiKey={apiKey}
        onComplete={handleComplete}
      />
    )
  })
}
