// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AdoptHostnameRecord,
  DnsRecordChange,
  DnsRecordConflict,
  SkipHostnameRecord,
} from '@/api/client'

/**
 * What the user chose for one conflicting generated hostname: adopt the
 * record at its name, or leave the hostname untouched.
 */
export type ConflictDecision = 'adopt' | 'skip'

/** Decisions by {@link conflictKey}. */
export type ConflictDecisions = Readonly<Record<string, ConflictDecision>>

/** Stable key of a conflict: its record type and hostname. */
export function conflictKey(
  conflict: Pick<DnsRecordConflict, 'name' | 'record_type'>
): string {
  return `${conflict.record_type} ${conflict.name.toLowerCase()}`
}

/**
 * Whether `decision` resolves `conflict`. Adopting needs a conflict the
 * server marked adoptable, with the record it would adopt.
 */
function resolves(
  conflict: DnsRecordConflict,
  decision: ConflictDecision | undefined
): boolean {
  if (decision === 'skip') return true
  return (
    decision === 'adopt' && conflict.adoptable && conflict.current_value != null
  )
}

/** Conflicts that still need a decision before the apply can run. */
export function unresolvedConflicts(
  conflicts: readonly DnsRecordConflict[],
  decisions: ConflictDecisions
): DnsRecordConflict[] {
  return conflicts.filter(
    (conflict) => !resolves(conflict, decisions[conflictKey(conflict)])
  )
}

/**
 * The apply request's `adopt_records` and `skip_records` for the conflicts
 * the user decided on. Each decision names the revision of the conflict the
 * user reviewed, so the server refuses it if the conflict changed since.
 */
export function conflictDecisionsRequest(
  conflicts: readonly DnsRecordConflict[],
  decisions: ConflictDecisions
): {
  adopt_records: AdoptHostnameRecord[]
  skip_records: SkipHostnameRecord[]
} {
  const adopt_records: AdoptHostnameRecord[] = []
  const skip_records: SkipHostnameRecord[] = []
  for (const conflict of conflicts) {
    const decision = decisions[conflictKey(conflict)]
    if (!resolves(conflict, decision)) continue
    const reviewed = {
      name: conflict.name,
      record_type: conflict.record_type,
      revision: conflict.revision,
    }
    if (decision === 'adopt') adopt_records.push(reviewed)
    else skip_records.push(reviewed)
  }
  return { adopt_records, skip_records }
}

/**
 * DNS changes to list as the plan's writes. Conflicts are listed separately,
 * each with its adopt or skip choice.
 */
export function plannedDnsChanges(
  changes: readonly DnsRecordChange[]
): DnsRecordChange[] {
  return changes.filter((change) => change.action !== 'conflict')
}

/** A record value with how Cloudflare serves it. */
export function describeRecordValue(
  value: string,
  proxied: boolean | null | undefined
): string {
  return `${value} (${proxied ? 'proxied' : 'DNS only'})`
}
