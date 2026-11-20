// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { GroupAssignee } from '@/api/client/types.gen'

/** Who the group is assigned to, for its label and avatar. */
export function assigneeLabel(
  assignee: GroupAssignee | null | undefined,
  legacyAssignedTo?: string | null
): string {
  if (assignee) {
    return assignee.type === 'team' ? `${assignee.name} (team)` : assignee.name
  }
  return legacyAssignedTo || 'Unassigned'
}

/** Two letters for an assignee avatar. */
export function assigneeInitials(assignee: GroupAssignee): string {
  const words = assignee.name.trim().split(/\s+/).filter(Boolean)
  const letters =
    words.length > 1
      ? `${words[0]?.[0] ?? ''}${words[1]?.[0] ?? ''}`
      : assignee.name.slice(0, 2)
  return (letters || '?').toUpperCase()
}
