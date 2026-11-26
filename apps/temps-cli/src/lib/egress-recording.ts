// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Outbound connection recording, shared by `temps sandbox recording|outbound`
// and `temps environments recording|outbound`: the flag parsing, the status
// and the activity table. The shapes come from the generated client.
import type {
  EgressActivityPage,
  EgressActivityRecord,
  EgressOutcomeFilter,
  EgressRecordingStatus,
} from '../api/types.gen.js'
import { colors, header, icons, keyValue, newline, warning } from '../ui/output.js'
import { printTable } from '../ui/table.js'

export const OUTCOME_FILTERS: readonly EgressOutcomeFilter[] = ['allowed', 'refused', 'failed']

/** `--outcome`, or an error naming the accepted values. */
export function parseOutcomeFilter(value: string | undefined): EgressOutcomeFilter | undefined {
  if (value === undefined) return undefined
  const outcome = OUTCOME_FILTERS.find((candidate) => candidate === value.trim().toLowerCase())
  if (!outcome) {
    throw new Error(`Unknown --outcome '${value}'. Expected one of: ${OUTCOME_FILTERS.join(', ')}`)
  }
  return outcome
}

/** The change `--on`/`--off` asks for, if any; both at once is an error. */
export function recordingChange(options: { on?: boolean; off?: boolean }): boolean | undefined {
  if (options.on && options.off) throw new Error('--on and --off cannot be combined')
  if (options.on) return true
  if (options.off) return false
  return undefined
}

/** The destination of a record as a person reads it. */
export function describeDestination(record: EgressActivityRecord): string {
  if (record.relay) return `${record.relay} relay`
  const host = record.host ?? '(no destination)'
  const where = record.port ? `${host}:${record.port}` : host
  return record.method && record.path ? `${record.method} ${where}${record.path}` : where
}

export function printRecordingStatus(title: string, status: EgressRecordingStatus): void {
  newline()
  header(`${icons.info} ${title}`)
  keyValue('Recording', status.enabled ? colors.success('on') : 'off')
  keyValue('Can record', status.configured ? 'yes' : 'no')
  if (!status.configured && status.reason) {
    warning(status.reason)
    if (status.setup_path) keyValue('Set it up at', status.setup_path)
  }
  for (const node of status.nodes) {
    const name = node.node_name ?? `node ${node.node_id}`
    keyValue(`  ${name}`, node.recording ? 'records' : (node.reason ?? 'does not record'))
  }
  if (status.logs_path) keyValue('Logs', status.logs_path)
  newline()
}

export function printActivity(title: string, page: EgressActivityPage): void {
  newline()
  header(`${icons.info} ${title}`)
  printTable(page.records, [
    { header: 'Time', accessor: (r) => r.timestamp },
    { header: 'Outcome', accessor: (r) => r.outcome },
    { header: 'Destination', accessor: describeDestination },
    { header: 'Reason', accessor: (r) => r.reason ?? '' },
    { header: 'Count', accessor: (r) => r.count, align: 'right' },
  ])
  keyValue('Page', `${page.page} (${page.records.length} of ${page.total})`)
  if (page.has_more) keyValue('More', `--page ${page.page + 1}`)
  newline()
}
