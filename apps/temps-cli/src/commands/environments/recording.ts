// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps environments recording <environment>` and
// `temps environments outbound <environment>`: whether an environment's
// deployments record their outbound connections, and what they recorded.
import type { Command } from 'commander'
import { client, getErrorMessage } from '../../lib/api-client.js'
import {
  getEnvironmentEgressRecording,
  listEnvironmentEgressRecords,
  setEnvironmentEgressRecording,
} from '../../api/sdk.gen.js'
import { parseIntegerFlag } from '../../lib/command-kit.js'
import {
  parseOutcomeFilter,
  printActivity,
  printRecordingStatus,
  recordingChange,
} from '../../lib/egress-recording.js'
import { withSpinner } from '../../ui/spinner.js'
import { json, success } from '../../ui/output.js'
import { connect, resolveTarget } from './workload.js'

interface RecordingOptions {
  project?: string
  on?: boolean
  off?: boolean
  json?: boolean
}

interface OutboundOptions {
  project?: string
  page?: string
  perPage?: string
  outcome?: string
  json?: boolean
}

export function registerEnvironmentRecordingCommands(environments: Command): void {
  environments
    .command('recording <environment>')
    .description(
      "Show or set whether the environment's deployments record their outbound connections (destination, outcome, bytes; never headers or bodies). Takes effect on the next deployment"
    )
    .option('-p, --project <project>', 'Project slug')
    .option('--on', 'Record outbound connections')
    .option('--off', 'Stop recording')
    .option('--json', 'Output in JSON format')
    .action(recordingCmd)

  environments
    .command('outbound <environment>')
    .description("List the outbound connections a recording environment's deployments made, newest first")
    .option('-p, --project <project>', 'Project slug')
    .option('--page <n>', 'Page number (default 1)')
    .option('--per-page <n>', 'Records per page (default 20, max 100)')
    .option('--outcome <outcome>', 'Only allowed, refused or failed connections')
    .option('--json', 'Output in JSON format')
    .action(outboundCmd)
}

async function recordingCmd(environment: string, options: RecordingOptions): Promise<void> {
  const enabled = recordingChange(options)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const path = { project_id: target.projectId, environment_id: target.environmentId }
  const status = await withSpinner(
    enabled === undefined ? 'Fetching recording...' : 'Updating recording...',
    async () => {
      const { data, error } =
        enabled === undefined
          ? await getEnvironmentEgressRecording({ client, path })
          : await setEnvironmentEgressRecording({ client, path, body: { enabled } })
      if (error || !data) throw new Error(getErrorMessage(error))
      return data
    }
  )
  if (options.json) {
    json(status)
    return
  }
  const name = `${target.projectSlug}/${target.environmentSlug}`
  if (enabled !== undefined) {
    success(`Recording ${enabled ? 'on' : 'off'} for ${name}${enabled ? '; redeploy to start recording' : ''}`)
  }
  printRecordingStatus(`Outbound recording of ${name}`, status)
}

async function outboundCmd(environment: string, options: OutboundOptions): Promise<void> {
  const page = options.page === undefined ? undefined : parseIntegerFlag(options.page, '--page', 1)
  const perPage =
    options.perPage === undefined ? undefined : parseIntegerFlag(options.perPage, '--per-page', 1, 100)
  const outcome = parseOutcomeFilter(options.outcome)
  await connect()
  const target = await resolveTarget(options.project, environment)
  const result = await withSpinner('Fetching outbound connections...', async () => {
    const { data, error } = await listEnvironmentEgressRecords({
      client,
      path: { project_id: target.projectId, environment_id: target.environmentId },
      query: { page, per_page: perPage, outcome },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  printActivity(`Outbound connections of ${target.projectSlug}/${target.environmentSlug}`, result)
}
