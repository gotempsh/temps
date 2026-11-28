// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps sandbox recording <id>` and `temps sandbox outbound <id>`: whether
// a sandbox records its outbound connections, and what it recorded.
import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { getEgressRecording, listEgressRecords, setEgressRecording } from '../../api/sdk.gen.js'
import { parseIntegerFlag } from '../../lib/command-kit.js'
import {
  parseOutcomeFilter,
  printActivity,
  printRecordingStatus,
  recordingChange,
} from '../../lib/egress-recording.js'
import { withSpinner } from '../../ui/spinner.js'
import { json, success } from '../../ui/output.js'

interface RecordingOptions {
  on?: boolean
  off?: boolean
  json?: boolean
}

interface OutboundOptions {
  page?: string
  perPage?: string
  outcome?: string
  json?: boolean
}

export function registerSandboxRecordingCommands(sandbox: Command): void {
  sandbox
    .command('recording <id>')
    .description(
      'Show or set whether the sandbox records its outbound connections (destination, outcome, bytes; never headers or bodies)',
    )
    .option('--on', 'Record its outbound connections')
    .option('--off', 'Stop recording')
    .option('--json', 'Output as JSON')
    .action(recordingAction)

  sandbox
    .command('outbound <id>')
    .description('List the outbound connections a recording sandbox made, newest first')
    .option('--page <n>', 'Page number (default 1)')
    .option('--per-page <n>', 'Records per page (default 20, max 100)')
    .option('--outcome <outcome>', 'Only allowed, refused or failed connections')
    .option('--json', 'Output as JSON')
    .action(outboundAction)
}

async function recordingAction(id: string, options: RecordingOptions): Promise<void> {
  const enabled = recordingChange(options)
  await requireAuth()
  await setupClient()
  const status = await withSpinner(
    enabled === undefined ? 'Fetching recording...' : 'Updating recording...',
    async () => {
      const { data, error } =
        enabled === undefined
          ? await getEgressRecording({ client, path: { id } })
          : await setEgressRecording({ client, path: { id }, body: { enabled } })
      if (error || !data) throw new Error(`Sandbox ${id}: ${getErrorMessage(error)}`)
      return data
    },
  )
  if (options.json) {
    json(status)
    return
  }
  if (enabled !== undefined) success(`Recording ${enabled ? 'on' : 'off'} for ${id}`)
  printRecordingStatus(`Outbound recording of ${id}`, status)
}

async function outboundAction(id: string, options: OutboundOptions): Promise<void> {
  const page = options.page === undefined ? undefined : parseIntegerFlag(options.page, '--page', 1)
  const perPage =
    options.perPage === undefined ? undefined : parseIntegerFlag(options.perPage, '--per-page', 1, 100)
  const outcome = parseOutcomeFilter(options.outcome)
  await requireAuth()
  await setupClient()
  const result = await withSpinner('Fetching outbound connections...', async () => {
    const { data, error } = await listEgressRecords({
      client,
      path: { id },
      query: { page, per_page: perPage, outcome },
    })
    if (error || !data) throw new Error(`Sandbox ${id}: ${getErrorMessage(error)}`)
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  printActivity(`Outbound connections of ${id}`, result)
}
