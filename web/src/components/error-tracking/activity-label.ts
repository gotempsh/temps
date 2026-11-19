// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ErrorGroupActivityResponse } from '@/api/client/types.gen'

const STATUS_WORDS: Record<string, string> = {
  resolved: 'resolved',
  ignored: 'ignored',
  unresolved: 'reopened',
}

function groupList(ids: number[] | undefined): string {
  const list = (ids ?? []).map((id) => `#${id}`)
  return list.length > 0 ? list.join(', ') : 'another group'
}

function fingerprints(count: number | null | undefined): string {
  if (count === null || count === undefined) return ''
  return ` (${count} fingerprint${count === 1 ? '' : 's'})`
}

/**
 * One line describing a non-comment activity entry, without its actor:
 * "resolved the group in the next release", "merged #12, #13 into this group".
 */
export function describeActivity(entry: ErrorGroupActivityResponse): string {
  const details = entry.details ?? {}
  switch (entry.kind) {
    case 'comment':
      return 'commented'
    case 'status_changed': {
      const word = STATUS_WORDS[details.status ?? ''] ?? 'changed the status of'
      const suffix = details.in_next_release ? ' in the next release' : ''
      const bulk = details.bulk ? ' (bulk action)' : ''
      return `${word} the group${suffix}${bulk}`
    }
    case 'regressed': {
      const was = details.status === 'ignored' ? 'ignored' : 'resolved'
      const release = details.release ? ` in release ${details.release}` : ''
      return `A new event${release} reopened this ${was} group`
    }
    case 'assigned':
      if (!details.assignee) return 'unassigned the group'
      if (details.assignee.type === 'team') {
        return `assigned the group to ${details.assignee_name ?? `team #${details.assignee.id}`} (team)`
      }
      return `assigned the group to ${details.assignee_name ?? `user #${details.assignee.id}`}`
    case 'merged':
      return `merged ${groupList(details.group_ids)} into this group${fingerprints(details.fingerprints)}`
    case 'unmerged':
      return `took the merge with ${groupList(details.group_ids)} apart${fingerprints(details.fingerprints)}`
    case 'split':
      return `split fingerprints between this group and ${groupList(details.group_ids)}${fingerprints(details.fingerprints)}`
    default:
      return 'changed the group'
  }
}

/** Who did it: a name, "Someone" for a deleted user, nobody for an event. */
export function activityActor(
  entry: ErrorGroupActivityResponse
): string | null {
  if (entry.kind === 'regressed') return null
  return entry.actor_name ?? 'Someone'
}
