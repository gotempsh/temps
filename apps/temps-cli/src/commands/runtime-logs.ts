// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth, config, credentials } from '../config/store.js'
import { setupClient, client, normalizeApiUrl } from '../lib/api-client.js'
import { resolveProjectSlug } from '../config/resolve-project.js'
import { getProjectBySlug, listContainers, getEnvironments } from '../api/sdk.gen.js'
import { colors, info, warning, newline } from '../ui/output.js'
import { startSpinner, succeedSpinner, failSpinner } from '../ui/spinner.js'

interface RuntimeLogsOptions {
  project?: string
  environment: string
  container?: string
  tail: string
  timestamps?: boolean
  follow?: boolean
}

export function registerRuntimeLogsCommand(program: Command): void {
  program
    .command('runtime-logs')
    .alias('rlogs')
    .description('View runtime container logs (use -f to follow in real-time)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <env>', 'Environment name', 'production')
    .option('-c, --container <id>', 'Container ID (partial match supported)')
    .option('-n, --tail <lines>', 'Number of lines to tail', '1000')
    .option('-t, --timestamps', 'Show timestamps')
    .option('-f, --follow', 'Follow log output (stream in real-time)')
    .action(runtimeLogs)
}

async function runtimeLogs(options: RuntimeLogsOptions): Promise<void> {
  const apiKey = await requireAuth()
  await setupClient()

  const resolved = await resolveProjectSlug(options.project)

  if (!resolved) {
    warning('No project specified')
    info('Use: bunx @temps-sdk/cli runtime-logs --project <slug>')
    info('Or link this directory: bunx @temps-sdk/cli link <slug>')
    return
  }

  const projectName = resolved.slug

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(projectName)} (from ${resolved.source})`)
  }

  // Get project by slug
  startSpinner('Finding project...')
  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: projectName },
  })

  if (projectError || !projectData) {
    failSpinner(`Project "${projectName}" not found`)
    return
  }
  succeedSpinner(`Found project: ${projectData.name}`)

  // Get environment
  startSpinner('Finding environment...')
  const { data: environments, error: envError } = await getEnvironments({
    client,
    path: { project_id: projectData.id },
  })

  if (envError || !environments || environments.length === 0) {
    failSpinner('No environments found')
    return
  }

  const environment = environments.find(e => e.name === options.environment)
  if (!environment) {
    failSpinner(`Environment "${options.environment}" not found`)
    info(`Available environments: ${environments.map(e => e.name).join(', ')}`)
    return
  }
  succeedSpinner(`Found environment: ${environment.name}`)

  // Get containers
  startSpinner('Finding containers...')
  const { data: containersResponse, error: containersError } = await listContainers({
    client,
    path: {
      project_id: projectData.id,
      environment_id: environment.id,
    },
  })

  if (containersError || !containersResponse?.containers || containersResponse.containers.length === 0) {
    failSpinner('No running containers found')
    return
  }

  const containers = containersResponse.containers
  succeedSpinner(`Found ${containers.length} container(s)`)

  // Select container
  let selectedContainer = containers[0]
  if (!selectedContainer) {
    warning('No container available')
    return
  }

  if (options.container) {
    const match = containers.find(c =>
      c.container_id.startsWith(options.container!) ||
      c.container_name?.includes(options.container!)
    )
    if (!match) {
      warning(`Container "${options.container}" not found`)
      info('Available containers:')
      for (const c of containers) {
        console.log(`  - ${c.container_id.substring(0, 12)} (${c.container_name || 'unnamed'})`)
      }
      return
    }
    selectedContainer = match
  }

  newline()
  info(`Streaming logs for container: ${colors.bold(selectedContainer.container_name || selectedContainer.container_id.substring(0, 12))}`)
  info(`Container ID: ${colors.muted(selectedContainer.container_id)}`)
  newline()

  // Build WebSocket URL
  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const wsProtocol = apiUrl.startsWith('https') ? 'wss' : 'ws'
  // Remove protocol and any trailing slash, keep the path (e.g., /api)
  const urlWithoutProtocol = apiUrl.replace(/^https?:\/\//, '').replace(/\/$/, '')

  const follow = options.follow ?? false
  const params = new URLSearchParams()
  params.append('tail', options.tail)
  params.append('timestamps', String(options.timestamps ?? false))
  params.append('follow', String(follow))

  const wsUrl = `${wsProtocol}://${urlWithoutProtocol}/projects/${projectData.id}/environments/${environment.id}/containers/${selectedContainer.container_id}/logs?${params.toString()}`

  if (follow) {
    info(`Streaming logs (follow mode)...`)
  } else {
    info(`Fetching logs...`)
  }
  newline()

  // Connect via WebSocket
  await connectWebSocket(wsUrl, apiKey, follow)
}

function formatLogMessage(raw: string): void {
  // Docker log lines include trailing newlines; strip them so
  // console.log doesn't produce double-spaced output.
  const data = raw.replace(/\r?\n$/, '')

  // Try to parse as JSON for structured logs
  try {
    const parsed = JSON.parse(data)
    if (parsed.error) {
      console.log(colors.error(`ERROR: ${parsed.error}`))
      if (parsed.detail) {
        console.log(colors.muted(`  ${parsed.detail}`))
      }
    } else if (parsed.message) {
      console.log(parsed.message.replace(/\r?\n$/, ''))
    } else {
      console.log(data)
    }
  } catch {
    // Plain text log line
    console.log(data)
  }
}

async function connectWebSocket(url: string, apiKey: string, follow: boolean): Promise<void> {
  return new Promise((resolve) => {
    const ws = new WebSocket(url, {
      headers: {
        'Authorization': `Bearer ${apiKey}`,
      },
    } as any)

    let sigintHandler: (() => void) | null = null

    ws.onopen = () => {
      if (follow) {
        console.log(colors.success('✓ Connected to log stream'))
        console.log(colors.muted('─'.repeat(60)))
        console.log(colors.muted('Press Ctrl+C to stop'))
        console.log(colors.muted('─'.repeat(60)))
        console.log()
      }
    }

    ws.onmessage = (event) => {
      formatLogMessage(event.data.toString())
    }

    ws.onerror = (error) => {
      console.error(colors.error('WebSocket error:'), error)
    }

    ws.onclose = (event) => {
      // Clean up the SIGINT handler
      if (sigintHandler) {
        process.removeListener('SIGINT', sigintHandler)
      }

      if (follow) {
        console.log()
        console.log(colors.muted('─'.repeat(60)))
        if (event.code === 1000) {
          console.log(colors.info('Connection closed normally'))
        } else {
          console.log(colors.warning(`Connection closed (code: ${event.code})`))
          if (event.reason) {
            console.log(colors.muted(`Reason: ${event.reason}`))
          }
        }
      }
      resolve()
    }

    // Handle Ctrl+C gracefully (only relevant for follow mode, but register always)
    sigintHandler = () => {
      console.log()
      console.log(colors.muted('Closing connection...'))
      try {
        ws.close(1000, 'User requested close')
      } catch {
        // WebSocket may already be closed
      }
      // Force exit after a short delay in case ws.close doesn't trigger onclose
      setTimeout(() => {
        process.exit(0)
      }, 500)
    }
    process.on('SIGINT', sigintHandler)
  })
}
