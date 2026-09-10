// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, useCallback } from 'react'
import { render, Box, Text, Newline } from 'ink'
import Spinner from 'ink-spinner'
import { config, credentials } from '../config/store.js'
import { normalizeApiUrl, getWebUrl } from './api-client.js'

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

const TERMINAL_STATUSES = ['success', 'completed', 'deployed', 'failed', 'error', 'cancelled']
const SUCCESS_STATUSES = ['success', 'completed', 'deployed']
const FAILURE_STATUSES = ['failed', 'error', 'cancelled']

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
      return <Text color="green">✓</Text>
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

  // Clean up the message — strip leading emoji
  const message = entry.message.replace(/^[\u{1F300}-\u{1F9FF}\u{2600}-\u{26FF}\u{2700}-\u{27BF}]\s*/u, '')

  return (
    <Box marginLeft={4}>
      <Text color={color}>{icon} {message}</Text>
    </Box>
  )
}

// Job row component
function JobRow({ jobState, isFinished }: { jobState: JobState; isFinished?: boolean }) {
  const { job, logs } = jobState
  const duration = job.started_at
    ? formatDuration(job.started_at, job.finished_at ?? undefined)
    : ''

  const statusColor = getStatusColor(job.status)
  // Show fewer logs when finished to keep output compact
  const maxLogs = isFinished ? 3 : (job.status === 'running' ? 10 : 5)
  const recentLogs = logs.slice(-maxLogs)

  return (
    <Box flexDirection="column" marginLeft={2}>
      <Box>
        <StatusIcon status={job.status} />
        <Text color={statusColor}> {job.name}</Text>
        {duration && <Text color="gray"> ({duration})</Text>}
      </Box>

      {/* Error message */}
      {FAILURE_STATUSES.includes(job.status) && job.error_message && (
        <Box marginLeft={2}>
          <Text color="red">Error: {job.error_message}</Text>
        </Box>
      )}

      {/* Logs — show during progress, compact on finish */}
      {recentLogs.length > 0 && !isFinished && (
        <Box flexDirection="column" marginLeft={2}>
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
  projectName,
  apiUrl,
  apiKey,
  onComplete,
}: WatcherInternalProps) {
  const [deployment, setDeployment] = useState<DeploymentResponse | null>(null)
  const [jobStates, setJobStates] = useState<Map<string, JobState>>(new Map())
  const [startTime] = useState(Date.now())
  const [elapsed, setElapsed] = useState('0s')
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<WatchDeploymentResult | null>(null)

  // Update elapsed time
  useEffect(() => {
    if (result) return // Stop updating once finished
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
  }, [startTime, result])

  // Signal completion after result is rendered
  useEffect(() => {
    if (!result) return
    const timer = setTimeout(() => onComplete(result), 200)
    return () => clearTimeout(timer)
  }, [result, onComplete])

  // Fetch jobs helper
  const fetchJobs = useCallback(async (currentJobStates: Map<string, JobState>): Promise<Map<string, JobState>> => {
    const jobsRes = await fetch(
      `${apiUrl}/projects/${projectId}/deployments/${deploymentId}/jobs`,
      { headers: { Authorization: `Bearer ${apiKey}` } }
    )

    if (!jobsRes.ok) return currentJobStates

    const jobsData = (await jobsRes.json()) as { jobs: DeploymentJobResponse[] }
    const jobs = jobsData.jobs || []
    jobs.sort((a, b) => a.id - b.id)

    const newJobStates = new Map(currentJobStates)

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
                } catch { /* skip malformed log lines */ }
              }
              if (newLogs.length > 0) {
                state = { ...state, logs: [...state.logs, ...newLogs] }
              }
            }
          }
        } catch { /* skip log fetch errors */ }
      }

      newJobStates.set(job.job_id, state)
    }

    return newJobStates
  }, [apiUrl, apiKey, projectId, deploymentId])

  // Main polling effect
  useEffect(() => {
    let cancelled = false
    const timeoutMs = timeoutSecs * 1000
    let latestJobStates = jobStates

    async function poll() {
      while (!cancelled && Date.now() - startTime < timeoutMs) {
        try {
          // 1. Fetch deployment status
          const deploymentRes = await fetch(
            `${apiUrl}/projects/${projectId}/deployments/${deploymentId}`,
            { headers: { Authorization: `Bearer ${apiKey}` } }
          )

          let dep: DeploymentResponse | null = null

          if (deploymentRes.ok) {
            dep = (await deploymentRes.json()) as DeploymentResponse
            setDeployment(dep)
          } else {
            const errorText = await deploymentRes.text()
            setError(`API Error ${deploymentRes.status}: ${errorText.substring(0, 200)}`)
          }

          // 2. Always fetch jobs (so final states are captured)
          latestJobStates = await fetchJobs(latestJobStates)
          if (!cancelled) {
            setJobStates(latestJobStates)
          }

          // 3. Check terminal state AFTER jobs are updated
          if (dep && TERMINAL_STATUSES.includes(dep.status)) {
            if (FAILURE_STATUSES.includes(dep.status)) {
              setResult({
                success: false,
                deployment: dep,
                error: dep.cancelled_reason || 'Deployment failed',
              })
              return
            }

            // Success — deployment is in a terminal success state
            setResult({ success: true, deployment: dep })
            return
          }

          await new Promise((r) => setTimeout(r, 1500))
        } catch (err) {
          setError(`Exception: ${err instanceof Error ? err.message : String(err)}`)
          await new Promise((r) => setTimeout(r, 2000))
        }
      }

      // Timeout
      if (!cancelled) {
        setResult({ success: false, error: 'Timeout waiting for deployment to complete' })
      }
    }

    poll()
    return () => { cancelled = true }
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  const sortedJobs = Array.from(jobStates.values()).sort((a, b) => a.job.id - b.job.id)
  const statusColor = deployment ? getStatusColor(deployment.status) : 'gray'
  const isFinished = !!result
  const webUrl = getWebUrl()

  return (
    <Box flexDirection="column" paddingTop={1}>
      {/* Header */}
      <Box>
        <Text bold>{'  '}🚀 Deployment #{deploymentId}</Text>
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
      {error && !result && (
        <Box marginLeft={1} marginTop={1}>
          <Text color="red">Error: {error}</Text>
        </Box>
      )}

      <Newline />

      {/* Jobs */}
      {sortedJobs.map((jobState) => (
        <JobRow key={jobState.job.job_id} jobState={jobState} isFinished={isFinished} />
      ))}

      {sortedJobs.length === 0 && deployment && !result && (
        <Box marginLeft={2}>
          <Text color="gray">Waiting for jobs...</Text>
        </Box>
      )}

      {/* Result summary */}
      {result && (
        <>
          <Newline />
          {result.success ? (
            <Box flexDirection="column">
              <Box marginLeft={1}>
                <Text color="green" bold>✓ Deployment completed successfully!</Text>
              </Box>
              {deployment?.url && (
                <Box marginLeft={3}>
                  <Text>URL: </Text>
                  <Text color="cyan" bold>{deployment.url}</Text>
                </Box>
              )}
              {deployment?.environment?.domains?.[0] && (
                <Box marginLeft={3}>
                  <Text>Domain: </Text>
                  <Text color="cyan" bold>
                    {deployment.environment.domains[0].startsWith('http')
                      ? deployment.environment.domains[0]
                      : `https://${deployment.environment.domains[0]}`}
                  </Text>
                </Box>
              )}
            </Box>
          ) : (
            <Box flexDirection="column">
              <Box marginLeft={1}>
                <Text color="red" bold>✗ Deployment failed</Text>
              </Box>
              {result.error && (
                <Box marginLeft={3}>
                  <Text color="red">{result.error}</Text>
                </Box>
              )}
            </Box>
          )}
          {projectName && (
            <Box marginLeft={3}>
              <Text color="gray">Dashboard: {webUrl}/projects/{projectName}/deployments</Text>
            </Box>
          )}
          <Newline />
        </>
      )}
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
  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey() || ''

  if (!apiKey) {
    return { success: false, error: 'No API key found. Please run: temps login' }
  }

  return new Promise((resolve) => {
    const instance = render(
      <DeploymentWatcher
        {...options}
        apiUrl={apiUrl}
        apiKey={apiKey}
        onComplete={(res) => {
          // Give Ink time to render the final state, then unmount
          setTimeout(() => {
            instance.unmount()
            resolve(res)
          }, 300)
        }}
      />
    )
  })
}
