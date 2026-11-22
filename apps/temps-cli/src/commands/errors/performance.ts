// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors performance`: whether traces are checked for performance
// issues, and `errors list --type` to list them (ADR 111, D2).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getPerformanceIssueStatus } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { header, info, json, keyValue, warning, formatRelativeTime } from '../../ui/output.js'

export const ISSUE_TYPES = ['error', 'performance'] as const

/** Check an `--type` value. */
export function checkIssueType(raw: string): (typeof ISSUE_TYPES)[number] {
  const value = raw.trim().toLowerCase()
  if (!(ISSUE_TYPES as readonly string[]).includes(value)) {
    throw new Error(`Unknown issue type "${raw}". Use error or performance`)
  }
  return value as (typeof ISSUE_TYPES)[number]
}

interface StatusOptions {
  projectId: string
  json?: boolean
}

export function registerPerformanceCommands(errors: Command): void {
  errors
    .command('performance')
    .description('Show whether traces are checked for performance issues, and what is detected')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(statusAction)
}

async function statusAction(options: StatusOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }
  const status = await withSpinner('Loading performance issue status...', async () => {
    const { data, error } = await getPerformanceIssueStatus({ client, path: { project_id: projectId } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(status)
    return
  }
  header('Performance issues')
  if (!status.configured) {
    warning(status.reason ?? 'Not checked yet')
  }
  keyValue('Last check', status.last_run_at ? formatRelativeTime(status.last_run_at) : 'never')
  keyValue('Traces checked', String(status.traces_scanned))
  keyValue('Issues filed', String(status.issues_found))
  keyValue('Per check', `${status.traces_per_check} slowest traces every ${status.check_interval_minutes} minutes`)
  if (status.last_error) {
    warning(`The last check failed: ${status.last_error}`)
  }
  printTable(status.detectors, [
    { header: 'Detector', accessor: (row) => row.title },
    { header: 'Finds', accessor: (row) => row.description },
  ])
  info('List them with: temps errors list --project-id <id> --type performance')
}
